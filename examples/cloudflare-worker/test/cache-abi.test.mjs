import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { importRepository, repositoryRequest, repositoryResponse } from "../src/repository.mjs";
import { attach, drain, generated, key, module, prepare, requestBody, unframe } from "./cache-fixture.mjs";
import { fixtures, synthetic } from "./repository-fixture.mjs";

for (const repository of fixtures) {
  for (const version of [0, 1, 2]) {
    test(`cache ABI ${repository.format} v${version} preserves exact wire output across suspended misses and hits`, () => checkPreparedWire(repository, version));
  }
}

test("cache ABI handles maximum raw arenas and complete packs larger than ordinary output", () => {
  for (const format of ["sha1", "sha256"]) {
    const repository = synthetic(format);
    const { exports, memory, result } = prepare(repository);
    assert.equal(result, 1);
    assert.equal(exports.cache_capacity(), 147456);
    const raw = generated(exports, 65521);
    assert.ok(raw.length > exports.output_capacity());
    assert.ok(raw.length < exports.cache_capacity());
    assert.equal(attach(exports, raw, raw.length), 3);
    assert.deepEqual(unframe(drain(exports, 65536)), raw);
    assert.equal(exports.memory.buffer, memory);
    exports.stream_cancel();
    assert.equal(exports.stream_read(1), -2);
    assert.equal(exports.cache_ptr(), 0);
  }
});

test("corrupt, truncated, wrong-format, wrong-set and over-budget packs cancel before response output", () => {
  for (const repository of fixtures) {
    const original = prepare(repository);
    const raw = generated(original.exports);
    const altered = Buffer.from(raw);
    altered[20] ^= 1;
    const other = prepare(fixtures.find((value) => value.format !== repository.format));
    const wrongFormat = generated(other.exports);
    const subset = prepare(repository, { extra: ["filter blob:none"] });
    const wrongSet = generated(subset.exports);
    const cases = [Buffer.alloc(0), raw.subarray(0, -1), Buffer.concat([raw, Buffer.from([0])]), altered, wrongFormat, wrongSet];
    for (const bytes of cases) {
      rejectCorruptPack(repository, bytes);
    }
    const limited = prepare(repository);
    assert.equal(attach(limited.exports, raw, raw.length - 1), -1);
    assert.equal(limited.exports.stream_read(1), -2);
    const oversized = prepare(repository);
    assert.equal(oversized.exports.cache_attach(147457, 147456), -1);
    assert.equal(oversized.exports.stream_read(1), -2);
  }
});

test("cache keys depend on exact selected objects rather than protocol, sideband or provider order", () => {
  for (const repository of fixtures) {
    const first = prepare(repository, { version: 0, band: "" });
    const identity = key(first.exports);
    const raw = generated(first.exports);
    for (const options of [{ version: 0, band: "side-band" }, { version: 1 }, { version: 2 }]) {
      const other = prepare({ ...repository, objects: [...repository.objects].reverse() }, options);
      assert.equal(key(other.exports), identity);
      assert.equal(attach(other.exports, raw), 3);
      assert.deepEqual(unframe(drain(other.exports)), raw);
    }
    const filtered = prepare(repository, { extra: ["filter blob:none"] });
    assert.notEqual(key(filtered.exports), identity);
    assert.equal(attach(filtered.exports, raw), -2);
    const duplicate = prepare(repository, { wants: [repository.head.id, repository.head.id] });
    assert.equal(key(duplicate.exports), identity);
  }
});

test("warmed bytes cannot authorize hidden wants, excessive duplicate wants or exhausted revision budgets", () => {
  for (const repository of fixtures) {
    const visibleBlob = repository.objects.find((value) => value.kind === "blob");
    const granted = prepare(repository, { wants: [visibleBlob.id], policy: 1 });
    assert.equal(granted.result, 1);
    generated(granted.exports);
    const strict = prepare(repository, { wants: [visibleBlob.id] });
    assert.equal(strict.result, -2);
    assert.equal(strict.exports.cache_key(), -2);
    assert.equal(strict.exports.stream_read(1), -2);
    const duplicates = prepare(repository, { wants: [repository.head.id, repository.head.id], maxWants: 1 });
    assert.equal(duplicates.result, -1);
    const exhausted = prepare(repository, { revisions: true, graphReadLimit: 0, extra: ["deepen-since 1"] });
    assert.equal(exhausted.result, -1);
    assert.equal(exhausted.exports.cache_ptr(), 0);
  }
});

test("new starts, imports and input reuse invalidate prepared cache state and recover safely", () => {
  const repository = fixtures[0];
  for (const action of [
    (exports) => exports.stream_cancel(),
    (exports) => exports.input_ptr(),
    (exports) => exports.repository_begin(0),
    (exports) => exports.stream_start_repository(0, 0),
    (exports) => exports.cache_prepare(0, 1, 2, 64, 0, 0, 0),
    (exports) => exports.cache_prepare(0, 1, 0, 64, 2, 0, 0),
  ]) {
    const { exports, memory } = prepare(repository);
    generated(exports);
    action(exports);
    assert.equal(exports.cache_key(), -2);
    assert.equal(exports.cache_read(1), -2);
    assert.equal(exports.stream_read(1), -2);
    importRepository(exports, repository);
    const body = requestBody(repository);
    const length = repositoryRequest(exports, "git-upload-pack", body, new Headers({ "Git-Protocol": "version=2", "Content-Type": "application/x-git-upload-pack-request" }));
    assert.equal(exports.cache_prepare(length, 1, 0, 64, 0, 0, 0), 1);
    const raw = generated(exports);
    assert.equal(attach(exports, raw), 3);
    assert.deepEqual(unframe(drain(exports)), raw);
    assert.equal(exports.memory.buffer, memory);
  }
});

test("overlapping instances preserve prepared keys, raw generation and attached output independently", () => {
  const first = prepare(fixtures[0]);
  const second = prepare(fixtures[1]);
  const identity = key(first.exports);
  const other = generated(second.exports);
  assert.equal(attach(second.exports, other), 3);
  assert.equal(key(first.exports), identity);
  const raw = generated(first.exports);
  assert.equal(attach(first.exports, raw), 3);
  assert.deepEqual(unframe(drain(second.exports, 3)), other);
  second.exports.stream_cancel();
  assert.deepEqual(unframe(drain(first.exports, 11)), raw);
  assert.equal(first.exports.memory.buffer, first.memory);
  assert.equal(second.exports.memory.buffer, second.memory);
});

function control(wire) {
  let offset = 0;
  while (offset < wire.length) {
    if (wire.subarray(offset, offset + 4).toString() === "PACK") break;
    const length = Number.parseInt(wire.subarray(offset, offset + 4).toString(), 16);
    if (length >= 4 && wire[offset + 4] === 1) break;
    offset += Math.max(4, length);
  }
  return wire.subarray(0, offset);
}

async function checkPreparedWire(repository, version) {
  const options = { version };
  const { exports, result, memory } = prepare(repository, options);
  assert.equal(result, 1);
  const identity = key(exports);
  assert.equal(identity.length, 96);
  assert.equal(exports.stream_read(1), -2, "no response output before attachment");
  assert.equal(exports.cache_read(0), -1);
  assert.equal(exports.cache_read(exports.output_capacity() + 1), -1);
  const raw = generated(exports, 17);
  assert.equal(key(exports), identity);
  assert.equal(raw.subarray(0, 4).toString(), "PACK");
  const width = repository.format === "sha1" ? 20 : 32;
  assert.deepEqual(createHash(repository.format).update(raw.subarray(0, -width)).digest(), raw.subarray(-width));
  assert.equal(attach(exports, raw), 3);
  assert.equal(exports.cache_ptr(), 0, "active pack bytes are read-only");
  assert.equal(exports.cache_read(1), -2);
  assert.equal(exports.cache_key(), -2);
  assert.equal(exports.stream_read(65537), -1, "attached output uses the old input arena");
  const metadata = Buffer.from(new Uint8Array(memory, exports.output_ptr(), 3));
  assert.equal(metadata.readUInt16LE(0), 200);
  assert.equal(metadata[2], 2);
  const miss = drain(exports, 1);
  assert.deepEqual(unframe(miss), raw);
  assert.equal(exports.memory.buffer, memory);
  const headers = new Headers({ "Git-Protocol": `version=${version}`, "Content-Type": "application/x-git-upload-pack-request" });
  const direct = await repositoryResponse(module, repository, "git-upload-pack", requestBody(repository, options), "POST", headers).arrayBuffer();
  assert.equal(Buffer.compare(unframe(miss), unframe(Buffer.from(direct))), 0);
  assert.deepEqual(control(miss), control(Buffer.from(direct)));
  const hit = prepare(repository, options);
  assert.equal(key(hit.exports), identity);
  assert.equal(attach(hit.exports, raw), 3);
  assert.deepEqual(drain(hit.exports, 1001), miss);
  assert.equal(hit.exports.memory.buffer, hit.memory);
}

function rejectCorruptPack(repository, bytes) {
  const { exports, memory } = prepare(repository);
  assert.equal(attach(exports, bytes), -2);
  assert.equal(exports.stream_read(1), -2);
  assert.equal(exports.cache_key(), -2);
  assert.equal(exports.cache_ptr(), 0);
  assert.equal(exports.memory.buffer, memory);
}
