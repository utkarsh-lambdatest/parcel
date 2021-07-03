// @flow

import {Optimizer} from '@parcel/plugin';
import {blobToString} from '@parcel/utils';
// import SourceMap from '@parcel/source-map';

import path from 'path';

const binding = require('../native');

export default (new Optimizer({
  async optimize({
    contents,
    map,
    bundle,
    options /*, getSourceMapReference */,
  }) {
    if (!bundle.env.shouldOptimize) {
      return {contents, map};
    }

    let code = await blobToString(contents);

    const result = binding.transform({
      filename: path.relative(
        options.projectRoot,
        path.join(bundle.target.distDir, bundle.name),
      ),
      code,
      source_maps: bundle.env.sourceMap != null,
    });

    let resultCode = result.code;
    let resultMap;
    // if (result.map) {
    //   resultMap = new SourceMap(options.projectRoot);
    //   resultMap.addVLQMap(JSON.parse(result.map));
    //   if (map) {
    //     resultMap.extends(map);
    //   }
    //   let sourcemapReference = await getSourceMapReference(resultMap);
    //   if (sourcemapReference) {
    //     resultCode += `\n//# sourceMappingURL=${sourcemapReference}\n`;
    //   }
    // }

    return {contents: resultCode, map: resultMap};
  },
}): Optimizer);
