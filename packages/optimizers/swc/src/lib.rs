extern crate napi;
#[macro_use]
extern crate napi_derive;
extern crate serde;
extern crate swc_common;
extern crate swc_ecma_minifier;
extern crate swc_ecmascript;

#[cfg(target_os = "macos")]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[cfg(windows)]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use napi::{CallContext, JsObject, JsUnknown, Result};

use serde::{Deserialize, Serialize};
use swc_common::comments::SingleThreadedComments;
use swc_common::errors::{DiagnosticBuilder, Emitter, Handler};
use swc_common::Globals;
use swc_common::{chain, sync::Lrc, FileName, Mark, SourceMap};
use swc_ecma_minifier::optimize;
// use swc_ecma_minifier::option::terser::TerserCompressorOptions;
// use swc_ecma_minifier::option::CompressOptions;
use swc_ecma_minifier::option::ExtraOptions;
// use swc_ecma_minifier::option::MangleOptions;
use swc_ecma_minifier::option::MinifyOptions;
use swc_ecmascript::ast::Module;
use swc_ecmascript::codegen::text_writer::JsWriter;
use swc_ecmascript::parser::lexer::Lexer;
use swc_ecmascript::parser::{EsConfig, PResult, Parser, StringInput, Syntax};
use swc_ecmascript::transforms::resolver::resolver_with_mark;
use swc_ecmascript::transforms::{compat::reserved_words::reserved_words, fixer, hygiene};

use swc_ecmascript::visit::FoldWith;

#[derive(Debug, Clone, Default)]
pub struct ErrorBuffer(std::sync::Arc<std::sync::Mutex<Vec<swc_common::errors::Diagnostic>>>);

impl Emitter for ErrorBuffer {
  fn emit(&mut self, db: &DiagnosticBuilder) {
    self.0.lock().unwrap().push((**db).clone());
  }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct SourceLocation {
  start_line: usize,
  start_col: usize,
  end_line: usize,
  end_col: usize,
}

impl SourceLocation {
  pub fn from(source_map: &swc_common::SourceMap, span: swc_common::Span) -> Self {
    let start = source_map.lookup_char_pos(span.lo);
    let end = source_map.lookup_char_pos(span.hi);
    // - SWC's columns are exclusive, ours are inclusive (column - 1)
    // - SWC has 0-based columns, ours are 1-based (column + 1)
    // = +-0
    SourceLocation {
      start_line: start.line,
      start_col: start.col_display + 1,
      end_line: end.line,
      end_col: end.col_display,
    }
  }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CodeHighlight {
  pub message: Option<String>,
  pub loc: SourceLocation,
}

#[derive(Serialize, Deserialize, Debug)]
struct Diagnostic {
  pub message: String,
  pub code_highlights: Option<Vec<CodeHighlight>>,
  pub hints: Option<Vec<String>>,
}

#[derive(Serialize, Debug, Deserialize)]
struct Config {
  filename: String,
  code: String,
  source_maps: bool,
}

#[derive(Serialize, Debug, Deserialize, Default)]
struct TransformResult {
  code: String,
  map: Option<String>,
  diagnostics: Option<Vec<Diagnostic>>,
}

#[js_function(1)]
fn transform(ctx: CallContext) -> Result<JsUnknown> {
  let opts = ctx.get::<JsObject>(0)?;
  let config: Config = ctx.env.from_js_value(opts)?;
  let mut result = TransformResult::default();

  let source_map = Lrc::new(SourceMap::default());
  let module = parse(config.code.as_str(), config.filename.as_str(), &source_map);

  match module {
    Err(err) => {
      let error_buffer = ErrorBuffer::default();
      let handler = Handler::with_emitter(true, false, Box::new(error_buffer.clone()));
      err.into_diagnostic(&handler).emit();

      let s = error_buffer.0.lock().unwrap().clone();
      let diagnostics: Vec<Diagnostic> = s
        .iter()
        .map(|diagnostic| {
          let message = diagnostic.message();
          let span = diagnostic.span.clone();
          let suggestions = diagnostic.suggestions.clone();

          let span_labels = span.span_labels();
          let code_highlights = if !span_labels.is_empty() {
            let mut highlights = vec![];
            for span_label in span_labels {
              highlights.push(CodeHighlight {
                message: span_label.label,
                loc: SourceLocation::from(&source_map, span_label.span),
              });
            }

            Some(highlights)
          } else {
            None
          };

          let hints = if !suggestions.is_empty() {
            Some(
              suggestions
                .into_iter()
                .map(|suggestion| suggestion.msg)
                .collect(),
            )
          } else {
            None
          };

          Diagnostic {
            message,
            code_highlights,
            hints,
          }
        })
        .collect();

      result.diagnostics = Some(diagnostics);
      ctx.env.to_js_value(&result)
    }
    Ok((module, comments)) => swc_common::GLOBALS.set(&Globals::new(), || {
      let top_level_mark = Mark::fresh(Mark::root());
      let module = module.fold_with(&mut resolver_with_mark(top_level_mark));

      let module = optimize(
        module,
        Some(&comments),
        None,
        &MinifyOptions {
          rename: true,
          wrap: false,
          enclose: false,
          compress: Some(Default::default()),
          mangle: Some(Default::default()),
          ..Default::default()
        },
        &ExtraOptions { top_level_mark },
      );

      let program = {
        let mut passes = chain!(reserved_words(), hygiene(), fixer(Some(&comments)),);
        module.fold_with(&mut passes)
      };

      let (buf, mut src_map_buf) =
        emit(source_map.clone(), comments, &program, config.source_maps)?;
      if config.source_maps {
        let mut map_buf = vec![];
        if let Ok(_) = source_map
          .build_source_map(&mut src_map_buf)
          .to_writer(&mut map_buf)
        {
          result.map = Some(String::from_utf8(map_buf).unwrap());
        }
      }
      result.code = String::from_utf8(buf).unwrap();
      ctx.env.to_js_value(&result)
    }),
  }
}

fn parse(
  code: &str,
  filename: &str,
  source_map: &Lrc<SourceMap>,
) -> PResult<(Module, SingleThreadedComments)> {
  let source_file = source_map.new_source_file(FileName::Real(filename.into()), code.into());

  let comments = SingleThreadedComments::default();
  let syntax = {
    let mut esconfig = EsConfig::default();
    esconfig.dynamic_import = true;
    esconfig.export_default_from = true;
    esconfig.export_namespace_from = true;
    esconfig.import_meta = true;
    Syntax::Es(esconfig)
  };

  let lexer = Lexer::new(
    syntax,
    Default::default(),
    StringInput::from(&*source_file),
    Some(&comments),
  );

  let mut parser = Parser::new_from(lexer);
  match parser.parse_module() {
    Err(err) => Err(err),
    Ok(module) => Ok((module, comments)),
  }
}

fn emit(
  source_map: Lrc<SourceMap>,
  comments: SingleThreadedComments,
  program: &Module,
  source_maps: bool,
) -> Result<(Vec<u8>, Vec<(swc_common::BytePos, swc_common::LineCol)>)> {
  let mut src_map_buf = vec![];
  let mut buf = vec![];
  {
    let writer = Box::new(JsWriter::new(
      source_map.clone(),
      "\n",
      &mut buf,
      if source_maps {
        Some(&mut src_map_buf)
      } else {
        None
      },
    ));
    let config = swc_ecmascript::codegen::Config { minify: true };
    let mut emitter = swc_ecmascript::codegen::Emitter {
      cfg: config,
      comments: Some(&comments),
      cm: source_map.clone(),
      wr: writer,
    };

    emitter.emit_module(&program)?;
  }

  return Ok((buf, src_map_buf));
}

#[module_exports]
fn init(mut exports: JsObject) -> Result<()> {
  exports.create_named_method("transform", transform)?;

  Ok(())
}
