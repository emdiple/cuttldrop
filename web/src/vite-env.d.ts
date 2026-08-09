/// <reference types="vite/client" />

// zxing-wasm declares its plain `.wasm` subpath, while Vite's asset pipeline
// requires `?url`. Keep that bridge local and typed rather than falling back
// to a runtime CDN URL.
declare module "zxing-wasm/reader/zxing_reader.wasm?url" {
  const url: string;
  export default url;
}
