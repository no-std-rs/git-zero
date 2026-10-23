import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { createRepositoryWorker, importRepository, publicationPolicy, repositoryRequest } from "../src/repository.mjs";
import { withServer } from "./http.mjs";
import { consume, execute, loadRepository, module } from "./revision-fixture.mjs";
import { synthetic } from "./repository-fixture.mjs";

const oracle = fileURLToPath(new URL("../../../tests/server_packed.py", import.meta.url));
const headers = new Headers();
const path = (checksum, extension) => `objects/pack/pack-${checksum}.${extension}`;

for (const format of ["sha1", "sha256"]) {
  test(`real Git packed/mixed Worker dumb clones preserve empty trees and refs, ${format}`, { timeout: 120000 }, async () => {
    for (const state of ["symbolic", "detached", "unborn"]) {
      const root = await mkdtemp(join(tmpdir(), `git-zero-worker-packed-${format}-`));
      try {
        await execute("python3", [oracle, "prepare", "--root", root, "--object-format", format, "--state", state], { timeout: 30000 });
        const repository = await loadRepository(root, format);
        const blob = repository.objects.find((entry) => entry.kind === "blob");
        const loose = `objects/${blob.id.slice(0, 2)}/${blob.id.slice(2)}`;
        const worker = createRepositoryWorker(module, (request) => {
          const url = new URL(request.url);
          const prefix = /^\/(packed|mixed)\/repo\.git\//.exec(url.pathname);
          if (!prefix) return null;
          const target = url.pathname.slice(prefix[0].length);
          // A mixed layout has a subset of loose objects plus a complete pack.
          // Missing loose commits trigger Git's pack lookup; the exposed blob is
          // still independently fetched below through the ordinary loose path.
          if (prefix[1] === "mixed" && /^objects\/[a-f0-9]{2}\//.test(target) && target !== loose) return null;
          return { repository, target: target + url.search, publication: prefix[1] === "packed" ? "packed-only" : "mixed" };
        });
        await withServer((request) => worker.fetch(request), async (origin) => {
          const response = await fetch(`${origin}/mixed/repo.git/${loose}`);
          assert.equal(response.status, 200); assert.equal(response.headers.get("content-type"), "application/x-git-loose-object");
          await response.arrayBuffer();
          assert.equal((await fetch(`${origin}/packed/repo.git/${loose}`)).status, 404);
          const result = await execute("python3", [oracle, "compare", "--root", root, "--object-format", format, "--state", state, "--zero-url", origin, "--no-files"], { timeout: 60000 });
          assert.match(result.stdout, /packed\/mixed dumb clone/);
        });
      } catch (error) {
        console.error(`Retained Worker packed fixture: ${root}`); throw error;
      }
      await rm(root, { recursive: true, force: true });
    }
  });

  test(`packed Worker ABI retains fixed memory at exact raw capacity, ${format}`, () => {
    const repository = synthetic(format);
    const exports = new WebAssembly.Instance(module, {}).exports;
    const memory = exports.memory.buffer;
    assert.ok(memory.byteLength <= 24 * 65536);
    importRepository(exports, repository);
    const stage = (target) => repositoryRequest(exports, target, Buffer.alloc(0), headers);
    let length = stage("objects/info/packs");
    assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
    const size = exports.publication_length();
    const listing = consume(exports, 1).toString(); assert.equal(size, Buffer.byteLength(listing));
    const match = /^P pack-([a-f0-9]+)\.pack\n\n$/.exec(listing); assert.ok(match);
    const checksum = match[1]; assert.equal(checksum.length, format === "sha1" ? 40 : 64);
    length = stage(path(checksum, "pack"));
    assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
    const packSize = exports.publication_length(); const expected = consume(exports, 4096);
    assert.equal(packSize, expected.length); assert.ok(packSize > 131072);
    length = stage(path(checksum, "idx"));
    assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
    const indexSize = exports.publication_length(); const index = consume(exports, 1);
    assert.equal(indexSize, index.length); assert.deepEqual(index.subarray(0, 8), Buffer.from([255, 116, 79, 99, 0, 0, 0, 2]));
    for (let i = 0; i < 16; i++) {
      length = stage(path(checksum, "pack"));
      assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
      assert.equal(exports.stream_read(1), 1); exports.stream_cancel();
      assert.equal(exports.stream_read(1), -2); assert.equal(exports.publication_length(), -1);
      length = stage(path(checksum, "pack"));
      assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
      assert.deepEqual(consume(exports, 7), expected);
      length = stage(path(checksum, "pack"));
      assert.equal(exports.stream_start_repository(length, 0), 3);
      assert.equal(new DataView(exports.memory.buffer, exports.output_ptr(), 3).getUint16(0, true), 404);
      consume(exports); assert.equal(exports.publication_length(), -1);
      length = stage(path(checksum, "pack"));
      assert.equal(exports.stream_start_repository_packed(length, 0, 99), -2);
      assert.equal(exports.stream_read(1), -2); assert.equal(exports.publication_length(), -1);
      assert.equal(exports.memory.buffer, memory);
    }
    length = stage(path(checksum, "pack"));
    assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
    const second = new WebAssembly.Instance(module, {}).exports;
    importRepository(second, repository);
    const other = repositoryRequest(second, path(checksum, "idx"), Buffer.alloc(0), headers);
    assert.equal(second.stream_start_repository_packed(other, 0, 0), 3); second.stream_cancel();
    assert.deepEqual(consume(exports), expected);
    length = stage("objects/info/packs");
    assert.equal(exports.stream_start_repository_packed(length, 0, 0), 3);
    // Starting a new import cancels the old publication before overwriting its
    // provider/metadata; a subsequent request must authenticate the new graph.
    importRepository(exports, repository);
    assert.equal(exports.stream_read(1), -2); assert.equal(exports.publication_length(), -1);
    assert.equal(exports.memory.buffer, memory);
  });
}

test("publication policy stays opt-in and rejects invalid modes or missing ABI support", () => {
  assert.equal(publicationPolicy(module), null);
  for (const mode of ["packed-only", "mixed"]) assert.equal(publicationPolicy(module, mode), mode);
  for (const mode of [null, "", true, {}, "packed", 1]) assert.throws(() => publicationPolicy(module, mode), TypeError);
  const emptyModule = new WebAssembly.Module(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]));
  assert.equal(publicationPolicy(emptyModule), null);
  assert.throws(() => publicationPolicy(emptyModule, "packed-only"), /stream_start_repository_packed/);
});
