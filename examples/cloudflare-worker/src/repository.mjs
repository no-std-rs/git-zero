import { readRequestBody } from "./bytes.mjs";
import { streamResponse } from "./stream.mjs";
import { wantPolicy } from "./want-policy.mjs";

const encoder = new TextEncoder();
const KINDS = { commit: 1, tree: 2, blob: 3, tag: 4 };
const MAX_OBJECTS = 64;
const MAX_REFS = 8;
const MAX_BYTES = 131072;

class RepositoryError extends Error {
  constructor(status = 502) { super("Invalid or oversized repository"); this.status = status; }
}

class Manifest {
  constructor(format) {
    if (!["sha1", "sha256"].includes(format)) throw new RepositoryError();
    this.width = format === "sha1" ? 40 : 64;
    this.chunks = [encoder.encode("GZR\0")];
    this.bytes = 4;
    this.names = 0;
    this.word(1);
    this.word(1);
    this.word(format === "sha1" ? 0 : 1);
  }
  add(bytes) {
    this.bytes += bytes.length;
    if (this.bytes > 8192) throw new RepositoryError(413);
    this.chunks.push(bytes);
  }
  word(value) {
    if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff) throw new RepositoryError();
    const bytes = new Uint8Array(4);
    new DataView(bytes.buffer).setUint32(0, value, true);
    this.add(bytes);
  }
  name(value) {
    if (typeof value !== "string") throw new RepositoryError();
    const bytes = encoder.encode(value);
    this.names += bytes.length;
    if (this.names > 4096) throw new RepositoryError(413);
    this.word(bytes.length);
    this.add(bytes);
  }
  id(value) {
    if (typeof value !== "string" || value.length !== this.width || !/^[0-9a-f]+$/.test(value)) throw new RepositoryError();
    this.add(Uint8Array.from(value.match(/../g), (pair) => Number.parseInt(pair, 16)));
  }
  head(value) {
    const kinds = ["unborn", "symbolic", "detached"];
    const kind = kinds.indexOf(value?.type);
    if (kind < 0) throw new RepositoryError();
    this.word(kind);
    if (kind !== 0) this.id(value.id);
    if (kind !== 2) this.name(value.target);
  }
  object(value) {
    const kind = KINDS[value.kind];
    if (!kind || !(value.body instanceof Uint8Array)) throw new RepositoryError();
    this.word(kind);
    this.word(value.body.length);
    this.id(value.id);
  }
  reference(value) {
    this.name(value.name);
    this.id(value.id);
    this.name(value.symbolicTarget ?? "");
    this.word(value.peeled ? 1 : 0);
    if (value.peeled) this.id(value.peeled);
  }
  finish() {
    const output = new Uint8Array(this.bytes);
    let offset = 0;
    for (const chunk of this.chunks) { output.set(chunk, offset); offset += chunk.length; }
    return output;
  }
}

export function repositoryManifest(repository) {
  if (!Array.isArray(repository.objects) || !Array.isArray(repository.refs)) throw new RepositoryError();
  if (repository.objects.length > MAX_OBJECTS || repository.refs.length > MAX_REFS) throw new RepositoryError(413);
  const manifest = new Manifest(repository.format);
  manifest.word(repository.objects.length);
  manifest.word(repository.refs.length);
  manifest.head(repository.head);
  let bytes = 0;
  for (const object of repository.objects) {
    manifest.object(object);
    bytes += object.body.length;
    if (bytes > MAX_BYTES) throw new RepositoryError(413);
  }
  for (const reference of repository.refs) manifest.reference(reference);
  return manifest.finish();
}

function stage(exports, bytes) {
  if (bytes.length > exports.input_capacity()) throw new RepositoryError(413);
  new Uint8Array(exports.memory.buffer, exports.input_ptr(), bytes.length).set(bytes);
}

function imported(result) {
  if (result < 0) throw new RepositoryError(result === -1 ? 413 : 502);
}

export function importRepository(exports, repository) {
  const manifest = repositoryManifest(repository);
  stage(exports, manifest);
  imported(exports.repository_begin(manifest.length));
  for (const object of repository.objects) {
    for (let offset = 0; offset < object.body.length; offset += 32768) {
      const chunk = object.body.subarray(offset, offset + 32768);
      stage(exports, chunk);
      imported(exports.repository_write(chunk.length));
    }
  }
  imported(exports.repository_finish());
}

export function repositoryRequest(exports, target, body, headers) {
  if (typeof target !== "string") throw new RepositoryError();
  const path = encoder.encode(target);
  const metadata = [headers.get("git-protocol") ?? "", headers.get("content-type") ?? ""].map((value) => encoder.encode(value));
  const length = 4 + path.length + 8 + metadata[0].length + metadata[1].length + body.length;
  if (length > exports.input_capacity()) throw new RepositoryError(413);
  const input = new Uint8Array(exports.memory.buffer, exports.input_ptr(), length);
  const view = new DataView(input.buffer, input.byteOffset, length);
  view.setUint32(0, path.length, true);
  input.set(path, 4);
  let offset = 4 + path.length;
  metadata.forEach((value, index) => view.setUint32(offset + index * 4, value.length, true));
  offset += 8;
  for (const value of metadata) { input.set(value, offset); offset += value.length; }
  input.set(body, offset);
  return length;
}

function failure(status, message) {
  return new Response(`${message}\n`, { status, headers: { "Cache-Control": "no-store" } });
}

export function repositoryResponse(module, repository, target, body, method, headers, policy = null) {
  const { exports } = new WebAssembly.Instance(module, {});
  importRepository(exports, repository);
  let length;
  try { length = repositoryRequest(exports, target, body, headers); }
  catch (error) {
    if (error instanceof RepositoryError && error.status === 413) return failure(413, "Request exceeds Wasm capacity");
    throw error;
  }
  const result = policy
    ? exports.stream_start_repository_policy(length, method === "POST" ? 1 : 0, 1, policy.maxWantLines)
    : exports.stream_start_repository(length, method === "POST" ? 1 : 0);
  return streamResponse(exports, result);
}

// The trusted loader returns { repository, target, wantPolicy?, maxWantLines? },
// or null. Authentication, public projection and immutable URL retention belong
// to its host. Policy scalars are copied before awaiting the request body.
export function createRepositoryWorker(module, loadRepository) {
  if (typeof loadRepository !== "function") throw new TypeError("A repository loader is required");
  return {
    async fetch(request) {
      if (!["GET", "POST"].includes(request.method)) return failure(405, "Method not allowed");
      let selected;
      try { selected = await loadRepository(request); }
      catch { return failure(503, "Repository unavailable"); }
      if (!selected) return failure(404, "Repository not found");
      let policy;
      try { policy = wantPolicy(module, selected.wantPolicy, selected.maxWantLines, "stream_start_repository_policy"); }
      catch { return failure(503, "Invalid host want policy or missing Wasm export"); }
      let body;
      try { body = await readRequestBody(request, 32768); }
      catch { return failure(400, "Invalid or oversized request"); }
      try {
        return repositoryResponse(module, selected.repository, selected.target, body, request.method, request.headers, policy);
      } catch (error) {
        return failure(error instanceof RepositoryError ? error.status : 502, "Invalid or oversized repository");
      }
    },
  };
}
