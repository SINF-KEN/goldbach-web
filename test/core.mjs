// The WASM core (client/core.js + core.wasm) in Node.js: the build targets
// browsers and workers, which fetch core.wasm; here the bytes come from disk
// through Emscripten's instantiateWasm hook. dir: a checkout's client/ directory.
import { readFile } from "node:fs/promises";
import { pathToFileURL, fileURLToPath } from "node:url";

const here = fileURLToPath(new URL("../client", import.meta.url));

export async function loadCore(dir = here) {
  const createCore = (await import(pathToFileURL(`${dir}/core.js`).href)).default;
  const bytes = await readFile(`${dir}/core.wasm`);
  return createCore({
    instantiateWasm(imports, done) {
      WebAssembly.instantiate(bytes, imports).then((r) => done(r.instance));
      return {};
    },
  });
}
