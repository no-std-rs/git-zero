import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { repositoryManifest, repositoryRequest } from "../src/repository.mjs";
import { fixtures, object, synthetic } from "./repository-fixture.mjs";

const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
const INPUT = 65536;
const OUTPUT = 131072;

function instance() {
  const { exports } = new WebAssembly.Instance(module, {});
  assert.ok(exports.memory.buffer.byteLength <= 24 * 65536);
  assert.equal(exports.input_capacity(), INPUT);
  assert.equal(exports.output_capacity(), OUTPUT);
  assert.equal(exports.repository_capacity(), 131072);
  assert.equal(exports.repository_manifest_capacity(), 8192);
  assert.deepEqual(WebAssembly.Module.imports(module), []);
  assert.ok(!WebAssembly.Module.exports(module).some(({ name }) => /alloc|free/.test(name)));
  return exports;
}

function stage(exports, bytes) {
  assert.ok(bytes.length <= INPUT);
  new Uint8Array(exports.memory.buffer, exports.input_ptr(), bytes.length).set(bytes);
}

function load(exports, repository, chunk = 65521, expected = 0) {
  const memory = exports.memory.buffer;
  const manifest = repositoryManifest(repository);
  stage(exports, manifest);
  assert.equal(exports.repository_begin(manifest.length), 0);
  const bytes = Buffer.concat(repository.objects.map((value) => value.body));
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    const part = bytes.subarray(offset, offset + chunk);
    stage(exports, part);
    assert.equal(exports.repository_write(part.length), 0);
    assert.equal(exports.memory.buffer, memory);
  }
  assert.equal(exports.repository_finish(), expected);
  assert.equal(exports.memory.buffer, memory);
}

function start(exports, target = "HEAD", body = Buffer.alloc(0), protocol = "") {
  const headers = new Headers({ "Git-Protocol": protocol, "Content-Type": "application/x-git-upload-pack-request" });
  const length = repositoryRequest(exports, target, body, headers);
  const result = exports.stream_start_repository(length, body.length ? 1 : 0);
  if (result < 0) return result;
  assert.equal(result, 3);
  const metadata = Buffer.from(new Uint8Array(exports.memory.buffer, exports.output_ptr(), 3));
  return { status: metadata.readUInt16LE(0), type: metadata[2] };
}

function drain(exports, chunk = 127) {
  const memory = exports.memory.buffer;
  const output = [];
  while (true) {
    const count = exports.stream_read(chunk);
    assert.ok(count >= 0 && count <= chunk);
    assert.equal(exports.memory.buffer, memory);
    if (count === 0) return Buffer.concat(output);
    output.push(Buffer.from(new Uint8Array(memory, exports.output_ptr(), count)));
  }
}

const packet = (line) => Buffer.from(`${(Buffer.byteLength(line) + 4).toString(16).padStart(4, "0")}${line}`);
function fetch(repository) {
  return Buffer.concat([packet("command=fetch\n"), packet(`object-format=${repository.format}\n`), Buffer.from("0001"), packet(`want ${repository.head.id}\n`), packet("done\n"), Buffer.from("0000")]);
}
function pack(wire) {
  const chunks = [];
  while (wire.length) {
    const length = Number.parseInt(wire.subarray(0, 4).toString(), 16);
    if (!length) break;
    const data = wire.subarray(4, length);
    if (data[0] === 1) chunks.push(data.subarray(1));
    wire = wire.subarray(length);
  }
  return Buffer.concat(chunks);
}

test("versioned raw imports cross INPUT and object boundaries in both formats without memory growth", () => {
  const exports = instance();
  const memory = exports.memory.buffer;
  for (const repository of [fixtures[0], fixtures[1], fixtures[0]]) {
    load(exports, repository, 8191);
    assert.equal(start(exports).status, 200);
    assert.equal(drain(exports).toString(), `ref: ${repository.head.target}\n`);
    assert.equal(start(exports, "git-upload-pack", fetch(repository), "version=2").status, 200);
    const encoded = pack(drain(exports));
    const width = repository.format === "sha1" ? 20 : 32;
    assert.equal(encoded.subarray(0, 4).toString(), "PACK");
    assert.deepEqual(createHash(repository.format).update(encoded.subarray(0, -width)).digest(), encoded.subarray(-width));
    assert.equal(exports.memory.buffer, memory);
  }
});

test("full 64-object/eight-ref/128KiB arenas stream beyond OUTPUT in both formats", () => {
  for (const format of ["sha1", "sha256"]) {
    const exports = instance();
    const repository = synthetic(format);
    load(exports, repository);
    assert.equal(start(exports, "git-upload-pack", fetch(repository), "version=2").status, 200);
    const encoded = pack(drain(exports, 1021));
    assert.ok(encoded.length > OUTPUT);
    assert.equal(encoded.readUInt32BE(8), 64);
    const width = format === "sha1" ? 20 : 32;
    assert.deepEqual(createHash(format).update(encoded.subarray(0, -width)).digest(), encoded.subarray(-width));
    const oversized = { ...repository, objects: [...repository.objects, object(format, "blob", "extra")] };
    assert.throws(() => repositoryManifest(oversized));
    const manifest = Buffer.from(repositoryManifest(repository));
    const firstRecord = 28 + width + 4 + Buffer.byteLength(repository.head.target);
    manifest.writeUInt32LE(manifest.readUInt32LE(firstRecord + 4) + 1, firstRecord + 4);
    stage(exports, manifest);
    assert.equal(exports.repository_begin(manifest.length), -1, "Rust rejects a declared body sum one byte over capacity");
    assert.equal(start(exports), -2);
  }
});

test("import mutations and invalid read capacities cannot overwrite an active response", () => {
  const repository = fixtures[0];
  const exports = instance();
  load(exports, repository);
  assert.equal(start(exports, "git-upload-pack", fetch(repository), "version=2").status, 200);
  const expected = drain(exports);
  assert.equal(start(exports, "git-upload-pack", fetch(repository), "version=2").status, 200);
  assert.equal(exports.repository_write(1), -2);
  assert.equal(exports.repository_finish(), -2);
  assert.equal(exports.stream_read(0), -1);
  assert.equal(exports.stream_read(OUTPUT + 1), -1);
  assert.deepEqual(drain(exports), expected);
  start(exports);
  assert.equal(exports.repository_begin(0), -2);
  assert.equal(exports.stream_read(1), -2);
  assert.equal(start(exports), -2);
  load(exports, repository);
  assert.equal(start(exports).status, 200);
  exports.stream_cancel();
  assert.equal(exports.stream_read(1), -2);
});

test("malformed versions/counts/lengths invalidate old imports and recover without growth", () => {
  const exports = instance();
  const memory = exports.memory.buffer;
  const repository = fixtures[0];
  const manifest = Buffer.from(repositoryManifest(repository));
  const cases = [[4, 2, -2], [8, 2, -2], [12, 2, -2], [16, 65, -1], [20, 9, -1]];
  for (const [offset, value, error] of cases) {
    load(exports, repository);
    const invalid = Buffer.from(manifest);
    invalid.writeUInt32LE(value, offset);
    stage(exports, invalid);
    assert.equal(exports.repository_begin(invalid.length), error);
    assert.equal(start(exports), -2);
    assert.equal(exports.memory.buffer, memory);
  }
  for (const invalid of [manifest.subarray(0, -1), Buffer.concat([manifest, Buffer.from([0])])]) {
    stage(exports, invalid);
    assert.equal(exports.repository_begin(invalid.length), -2);
  }
  stage(exports, manifest);
  assert.equal(exports.repository_begin(manifest.length), 0);
  assert.equal(exports.repository_finish(), -2);
  assert.equal(start(exports), -2);
  load(exports, repository);
  assert.equal(exports.repository_begin(INPUT + 1), -1);
  assert.equal(start(exports), -2);
  load(exports, repository);
  assert.equal(start(exports).status, 200);
  assert.equal(exports.memory.buffer, memory);
});

test("wrong IDs, unreachable objects, broken refs and partial imports never become Ready", () => {
  for (const repository of fixtures) {
    const exports = instance();
    const wrong = { ...repository, objects: repository.objects.map((value, index) => index ? value : { ...value, id: "0".repeat(value.id.length) }) };
    const extra = { ...repository, objects: [...repository.objects, object(repository.format, "blob", "unreachable private body")] };
    const missing = { ...repository, objects: repository.objects.slice(1) };
    const duplicate = { ...repository, objects: [...repository.objects.slice(0, -1), repository.objects[0]] };
    const cycle = { ...repository, refs: repository.refs.map((value, index) => index ? value : { ...value, symbolicTarget: value.name }) };
    for (const invalid of [wrong, extra, missing, duplicate, cycle]) {
      load(exports, invalid, 32749, -2);
      assert.equal(start(exports), -2);
    }
    const manifest = repositoryManifest(repository);
    stage(exports, manifest);
    assert.equal(exports.repository_begin(manifest.length), 0);
    assert.equal(exports.repository_write(INPUT + 1), -1);
    assert.equal(exports.repository_finish(), -2);
    load(exports, repository);
    assert.equal(start(exports).status, 200);
  }
});

test("empty/unborn imports, detached HEAD, and exact name-pool limits keep format and ownership", () => {
  for (const format of ["sha1", "sha256"]) {
    const exports = instance();
    const target = "refs/heads/" + "a".repeat(4096 - "refs/heads/".length);
    const empty = { format, objects: [], refs: [], head: { type: "unborn", target } };
    load(exports, empty);
    assert.equal(start(exports).status, 200);
    assert.equal(drain(exports).toString(), `ref: ${target}\n`);
    assert.throws(() => repositoryManifest({ ...empty, head: { ...empty.head, target: target + "a" } }));
    const overName = Buffer.concat([repositoryManifest(empty), Buffer.from("a")]);
    overName.writeUInt32LE(4097, 28);
    stage(exports, overName);
    assert.equal(exports.repository_begin(overName.length), -1);
    assert.equal(start(exports), -2);
    const overManifest = Buffer.alloc(8193);
    stage(exports, overManifest);
    assert.equal(exports.repository_begin(overManifest.length), -1);
    load(exports, empty);
    assert.equal(start(exports, "info/refs?service=git-upload-pack", Buffer.alloc(0), "version=2").status, 200);
    assert.ok(drain(exports).includes(Buffer.from(`object-format=${format}\n`)));
    const fixture = fixtures.find((value) => value.format === format);
    load(exports, { ...fixture, head: { type: "detached", id: fixture.head.id } });
    assert.equal(start(exports).status, 200);
    assert.equal(drain(exports).toString(), `${fixture.head.id}\n`);
  }
});

test("extra body bytes invalidate the import and independent instances retain interleaved responses", () => {
  const first = instance();
  const second = instance();
  const repository = fixtures[0];
  const manifest = repositoryManifest(repository);
  stage(first, manifest);
  assert.equal(first.repository_begin(manifest.length), 0);
  for (const value of repository.objects) {
    for (let offset = 0; offset < value.body.length; offset += 32768) {
      const bytes = value.body.subarray(offset, offset + 32768);
      stage(first, bytes);
      assert.equal(first.repository_write(bytes.length), 0);
    }
  }
  stage(first, Buffer.from([0]));
  assert.equal(first.repository_write(1), -1);
  assert.equal(first.repository_finish(), -2);
  assert.equal(start(first), -2);
  load(first, repository);
  start(first, "git-upload-pack", fetch(repository), "version=2");
  const expected = drain(first);
  start(first, "git-upload-pack", fetch(repository), "version=2");
  const count = first.stream_read(13);
  const retained = Buffer.from(new Uint8Array(first.memory.buffer, first.output_ptr(), count));
  load(second, fixtures[1]);
  start(second, "git-upload-pack", fetch(fixtures[1]), "version=2");
  const other = pack(drain(second));
  assert.deepEqual(createHash("sha256").update(other.subarray(0, -32)).digest(), other.subarray(-32));
  assert.deepEqual(Buffer.concat([retained, drain(first)]), expected);
  first.stream_cancel();
  assert.equal(start(second).status, 200);
  assert.equal(drain(second).toString(), `ref: ${fixtures[1].head.target}\n`);
});

test("legacy and raw frames alternate without leaking imported refs or selection slots", () => {
  const exports = instance();
  const parts = ["{}\n", "{}\n", "{}\n", "{}\n"].map((value) => Buffer.from(value));
  const sizes = Buffer.alloc(16);
  parts.forEach((value, index) => sizes.writeUInt32LE(value.length, index * 4));
  const target = Buffer.from("info/refs?service=git-upload-pack");
  const size = Buffer.alloc(4);
  size.writeUInt32LE(target.length);
  const legacy = Buffer.concat([sizes, ...parts, size, target, Buffer.alloc(8)]);
  const legacyResponse = () => {
    stage(exports, legacy);
    assert.equal(exports.stream_start(legacy.length, 0, 0), 3);
    return drain(exports);
  };
  const expected = legacyResponse();
  for (const repository of [synthetic("sha256"), fixtures[0], fixtures[1]]) {
    load(exports, repository);
    start(exports, "git-upload-pack", fetch(repository), "version=2");
    assert.ok(pack(drain(exports)).length > 0);
    assert.deepEqual(legacyResponse(), expected);
  }
});
