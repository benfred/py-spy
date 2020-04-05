import {terser} from "rollup-plugin-terser";
import {default as config} from "./rollup.config.debug.js"

// Same basic rollup config as the debug version, just running terser to minify
// and not generating a sourcemap
config.output.file = "dist/release/bundle.js"
config.output.sourcemap = false;
config.plugins.push(terser({output: {preamble: config.output.banner}}));

export default config;
