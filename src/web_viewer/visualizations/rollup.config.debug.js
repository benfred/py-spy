import ascii from "rollup-plugin-ascii";
import typescript from "rollup-plugin-typescript2";
import commonjs from "rollup-plugin-commonjs";
import node from "rollup-plugin-node-resolve";

import * as meta from "./package.json";
const copyright = `// ${meta.homepage} v${meta.version} Copyright ${(new Date).getFullYear()} ${meta.author.name}`;

export default {
    input: "index.ts",
    output: {
        banner: copyright,
        file: "dist/debug/bundle.js",
        sourcemap: true,
        indent: false,
        extend: true,
        name: 'pyspy',
        format: "umd",
    },
    plugins: [
        typescript(),
        commonjs(),
        node(),
        ascii(),
    ],
    onwarn(warning, warn) {
        // silence circular dependency warnings in d3, since it seems to be by design
        // https://github.com/d3/d3-selection/issues/168
        if (warning.code === 'CIRCULAR_DEPENDENCY') return;
        warn(warning);
    }
}
