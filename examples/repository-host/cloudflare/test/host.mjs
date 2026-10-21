import { readFile } from "node:fs/promises";

// Node supplies the Wasm asset loader. The production entry's body is unchanged.
const entry = new URL("../src/streamed.mjs", import.meta.url);
let source = await readFile(entry, "utf8");
const asset = new URL("../build/git_zero_host_wasm.wasm", import.meta.url);
source = source.replace('import module from "../build/git_zero_host_wasm.wasm";', `import { readFile as readWasm } from "node:fs/promises"; const module = await WebAssembly.compile(await readWasm(new URL(${JSON.stringify(asset.href)})));`);
source = source.replace(/from "(\.\/[^\"]+)"/g, (_, path) => `from "${new URL(path, entry).href}"`);
source += `\n//# sourceURL=${entry.href}\n`;
const host = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
export const gateway = host.default;
export const OwnRepository = host.OwnRepository;
