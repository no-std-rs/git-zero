import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createPackCache } from "../src/pack-cache.mjs";
import { createRepositoryWorker, importRepository, repositoryRequest } from "../src/repository.mjs";

export const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
export const packet = (line) => Buffer.from(`${(Buffer.byteLength(line) + 4).toString(16).padStart(4, "0")}${line}`);

export function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

export function backend(kind = "kv") {
  const records = new Map(), operations = [];
  const state = { records, operations, getGate: null, putGate: null, readError: null, writeError: null, nextRead: undefined };
  return Object.assign(state, {
    async get(key, type) {
      operations.push({ operation: "get", key, type });
      if (state.getGate) await state.getGate.promise;
      if (state.readError) throw state.readError;
      const bytes = state.nextRead ?? records.get(key);
      state.nextRead = undefined;
      if (!bytes) return null;
      const stream = new ReadableStream({ start(controller) { controller.enqueue(bytes.slice()); controller.close(); } });
      return kind === "kv" ? stream : { size: bytes.length, body: stream };
    },
    async put(key, bytes, options) {
      operations.push({ operation: "put", key, options });
      if (state.putGate) await state.putGate.promise;
      if (state.writeError) throw state.writeError;
      records.set(key, new Uint8Array(bytes).slice());
    },
  });
}

export function cache(kind = "kv", options = {}) {
  const binding = backend(kind);
  const policy = createPackCache({ backend: kind, binding, namespace: "test-private", ...options });
  return { binding, policy };
}

export function requestBody(repository, { version = 2, wants = [repository.head.id], haves = [], extra = [], done = true, band = "side-band-64k" } = {}) {
  const parts = [];
  if (version === 2) parts.push(packet("command=fetch\n"), packet(`object-format=${repository.format}\n`), Buffer.from("0001"));
  wants.forEach((id, index) => parts.push(packet(`want ${id}${version !== 2 && index === 0 ? ` object-format=${repository.format}${extra.some((line) => line.startsWith("filter ")) ? " filter" : ""}${band ? ` ${band}` : ""}` : ""}\n`)));
  for (const line of extra) parts.push(packet(`${line}\n`));
  if (version !== 2) parts.push(Buffer.from("0000"));
  for (const id of haves) parts.push(packet(`have ${id}\n`));
  if (done) parts.push(packet("done\n"));
  if (version === 2 || !done && haves.length) parts.push(Buffer.from("0000"));
  return Buffer.concat(parts);
}

export function httpRequest(repository, options = {}) {
  const version = options.version ?? 2;
  return new Request("http://cache.invalid/repo.git/git-upload-pack", {
    method: "POST", body: requestBody(repository, options), signal: options.signal,
    headers: { "Content-Type": "application/x-git-upload-pack-request", "Git-Protocol": `version=${version}` },
  });
}

export function handler(repository, packCache, options = {}) {
  return createRepositoryWorker(module, async (request) => {
    const url = new URL(request.url);
    const match = /^\/repo\.git\/(.*)$/.exec(url.pathname);
    return match ? { repository, target: `${match[1]}${url.search}`, packCache, ...options } : null;
  });
}

export function prepare(repository, options = {}) {
  const { exports } = new WebAssembly.Instance(module, {});
  const memory = exports.memory.buffer;
  assert.ok(memory.byteLength <= 24 * 65536);
  importRepository(exports, repository);
  const version = options.version ?? 2;
  const body = requestBody(repository, options);
  const length = repositoryRequest(exports, "git-upload-pack", body,
    new Headers({ "Content-Type": "application/x-git-upload-pack-request", "Git-Protocol": `version=${version}` }));
  const result = exports.cache_prepare(length, 1, options.policy ?? 0, options.maxWants ?? 64,
    options.revisions ? 1 : 0, options.maxExclusions ?? 8, options.graphReadLimit ?? 1048576);
  return { exports, memory, result };
}

export function key(exports) {
  assert.equal(exports.cache_key(), 48);
  return Buffer.from(new Uint8Array(exports.memory.buffer, exports.output_ptr(), 48)).toString("hex");
}

export function generated(exports, chunk = 251) {
  const bytes = [];
  while (true) {
    const count = exports.cache_read(chunk);
    assert.ok(count >= 0 && count <= chunk);
    if (count === 0) return Buffer.concat(bytes);
    bytes.push(Buffer.from(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count)));
  }
}

export function attach(exports, pack, maximum = 147456) {
  const pointer = exports.cache_ptr();
  assert.ok(pointer);
  new Uint8Array(exports.memory.buffer, pointer, pack.length).set(pack);
  return exports.cache_attach(pack.length, maximum);
}

export function drain(exports, chunk = 257) {
  const bytes = [];
  while (true) {
    const count = exports.stream_read(chunk);
    assert.ok(count >= 0 && count <= chunk);
    if (count === 0) return Buffer.concat(bytes);
    bytes.push(Buffer.from(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count)));
  }
}

export function unframe(wire) {
  const raw = wire.indexOf(Buffer.from("PACK"));
  if (wire.subarray(0, 4).toString() === "PACK") return wire;
  const parts = [];
  let offset = 0;
  while (offset < wire.length) {
    const size = Number.parseInt(wire.subarray(offset, offset + 4).toString(), 16);
    assert.ok(Number.isSafeInteger(size));
    if (size < 4) { offset += 4; continue; }
    assert.ok(offset + size <= wire.length);
    const payload = wire.subarray(offset + 4, offset + size);
    if (payload[0] === 1) parts.push(payload.subarray(1));
    offset += size;
    if (!parts.length && wire.subarray(offset, offset + 4).toString() === "PACK") return wire.subarray(offset);
  }
  assert.ok(raw >= 0 || parts.length === 0);
  return Buffer.concat(parts);
}
