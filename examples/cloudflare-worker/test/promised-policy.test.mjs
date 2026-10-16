import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { gzipSync } from "node:zlib";
import { createRepositoryWorker, importRepository, repositoryRequest } from "../src/repository.mjs";
import { createWorker } from "../src/worker.mjs";
import { fixtures, object } from "./repository-fixture.mjs";
import { fetchBody, packet } from "./promised-oracle.mjs";
const bytes = await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url));
const module = await WebAssembly.compile(bytes);
function host(repository, options = {}, selectedModule = module) {
  return createRepositoryWorker(selectedModule, async request => ({ repository,
    target: new URL(request.url).pathname.slice(1) + new URL(request.url).search, ...options }));
}
function request(body, version = 0, gzip = false, suffix = "") {
  return new Request(`https://test.invalid/git-upload-pack${suffix}`, { method: "POST", headers: {
    "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}`,
    ...(gzip ? { "content-encoding": "gzip" } : {}), "x-want-policy": "all-visible-reachable",
  }, body: gzip ? gzipSync(body) : body });
}
const discover = (worker, version = 0) => worker.fetch(new Request("https://test.invalid/info/refs?service=git-upload-pack", { headers: { "git-protocol": `version=${version}` } }));
function consume(exports, width = 1) {
  const chunks = [];
  for (;;) {
    const count = exports.stream_read(width); assert.ok(count >= 0);
    if (count === 0) break;
    chunks.push(Buffer.from(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count)));
  }
  return Buffer.concat(chunks);
}

test("host projection and occurrence bounds authorize before output independently of wire hints", async () => {
  for (const repository of fixtures) {
    const blob = repository.objects.find(value => value.kind === "blob" && value.body.length === 98304);
    // Physical backing holds a private branch/blob and dangling blob; only the
    // immutable public descriptors are passed to the validated importer.
    const privateBlob = object(repository.format, "blob", "private body\n");
    const privateTree = object(repository.format, "tree", Buffer.concat([Buffer.from("100644 secret\0"), Buffer.from(privateBlob.id, "hex")]));
    const privateCommit = object(repository.format, "commit", `tree ${privateTree.id}\nauthor A <a@example.invalid> 0 +0000\ncommitter A <a@example.invalid> 0 +0000\n\nprivate\n`);
    const dangling = object(repository.format, "blob", "unreachable body\n");
    const backing = new Map([...repository.objects, privateBlob, privateTree, privateCommit, dangling].map(value => [value.id, value]));
    const projected = { ...repository, objects: repository.objects.map(value => backing.get(value.id)) };
    const strict = host(projected), enabled = host(projected, { wantPolicy: "all-visible-reachable", maxWantLines: 2 });
    for (const version of [0, 1, 2]) {
      assert.ok(!(await (await discover(strict, version)).text()).includes("allow-reachable-sha1-in-want"));
      assert.equal((await (await discover(enabled, version)).text()).includes("allow-reachable-sha1-in-want"), version !== 2);
      for (const gzip of [false, true]) {
        assert.equal((await strict.fetch(request(fetchBody(repository.format, version, [blob.id]), version, gzip, "?wantPolicy=all-visible-reachable"))).status, 400);
        const accepted = await enabled.fetch(request(fetchBody(repository.format, version, [blob.id], "blob:none"), version, gzip));
        assert.equal(accepted.status, 200); assert.ok((await accepted.arrayBuffer()).byteLength > blob.body.length);
        for (const id of [privateCommit.id, privateBlob.id, dangling.id]) {
          for (const ids of [[id], [blob.id, id], [id, blob.id]]) {
            const denied = await enabled.fetch(request(fetchBody(repository.format, version, ids), version, gzip));
            assert.equal(denied.status, 400); assert.equal(await denied.text(), "Invalid or oversized Git request\n");
          }
        }
      }
      const duplicate = fetchBody(repository.format, version, [blob.id, blob.id]);
      assert.equal((await host(projected, { wantPolicy: "all-visible-reachable", maxWantLines: 1 }).fetch(request(duplicate, version))).status, 413);
      const retry = await enabled.fetch(request(duplicate, version)); assert.equal(retry.status, 200); await retry.arrayBuffer();
      for (const id of ["0".repeat(blob.id.length), blob.id.length === 40 ? "a".repeat(64) : "a".repeat(40)]) {
        assert.equal((await enabled.fetch(request(fetchBody(repository.format, version, [id]), version))).status, 400);
      }
      const malformed = Buffer.concat([packet(`want ${blob.id} allow-reachable-sha1-in-want\n`), Buffer.from("0000"), packet("done\n")]);
      assert.equal((await enabled.fetch(request(malformed))).status, 400);
    }
    const invalid = host({ ...projected, objects: [...projected.objects, dangling] }, { wantPolicy: "all-visible-reachable" });
    assert.equal((await discover(invalid)).status, 502, "unreachable descriptors cannot be admitted");
  }
});

test("same-instance starts cancel and reset request-local permission without memory growth", () => {
  const first = new WebAssembly.Instance(module).exports, second = new WebAssembly.Instance(module).exports;
  const initial = first.memory.buffer, pages = initial.byteLength / 65536; assert.ok(pages <= 24);
  const headers = new Headers({ "content-type": "application/x-git-upload-pack-request" });
  for (const repository of fixtures) {
    const blob = repository.objects.find(value => value.kind === "blob" && value.body.length === 98304);
    importRepository(first, repository);
    const stage = () => repositoryRequest(first, "git-upload-pack", fetchBody(repository.format, 0, [blob.id]), headers);
    for (let round = 0; round < 4; round++) {
      let length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 1), 3);
      assert.ok(first.stream_read(1) > 0); first.stream_cancel(); assert.equal(first.stream_read(1), -2);
      length = stage(); assert.equal(first.stream_start_repository(length, 1), -2); assert.equal(first.stream_read(1), -2);
      length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 1), 3);
      assert.equal(first.stream_start_repository_policy(length, 1, 99, 1), -2); assert.equal(first.stream_read(1), -2);
      length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 0), -1); assert.equal(first.stream_read(1), -2);
      length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 1), 3); const expected = consume(first, 4096);
      length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 1), 3); assert.deepEqual(consume(first), expected);
      length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 1), 3);
      first.input_ptr(); assert.equal(first.stream_read(1), -2);
      length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 0, 1), -2);
      assert.equal(first.memory.buffer, initial);
    }
    const other = fixtures.find(value => value.format !== repository.format);
    importRepository(second, other);
    const otherLength = repositoryRequest(second, "git-upload-pack", fetchBody(repository.format, 0, [blob.id]), headers);
    assert.equal(second.stream_start_repository_policy(otherLength, 1, 1, 1), -2);
    const length = stage(); assert.equal(first.stream_start_repository_policy(length, 1, 1, 1), 3);
    second.stream_cancel(); assert.ok(consume(first).length > blob.body.length); assert.equal(first.memory.buffer, initial);
  }
  console.log(JSON.stringify({ promisedWasmBytes: bytes.length, pages, noGrowth: true }));
});

// Functional modules with an old/incomplete export surface; no fake strict
// implementation and no generated binaries are added to the source tree.
function withoutExports(names) {
  let offset = 8;
  function word() { let value = 0, shift = 0, byte; do { byte = bytes[offset++]; value |= (byte & 127) << shift; shift += 7; } while (byte & 128); return value; }
  const encode = value => { const output = []; do { let byte = value & 127; value >>>= 7; if (value) byte |= 128; output.push(byte); } while (value); return Buffer.from(output); };
  const sections = [bytes.subarray(0, 8)];
  while (offset < bytes.length) {
    const start = offset, kind = bytes[offset++], length = word(), end = offset + length;
    if (kind !== 7) sections.push(bytes.subarray(start, end));
    else {
      const count = word(), entries = [];
      for (let i = 0; i < count; i++) { const start = offset, size = word(), name = bytes.subarray(offset, offset + size).toString(); offset += size + 1; word(); if (!names.includes(name)) entries.push(bytes.subarray(start, offset)); }
      const body = Buffer.concat([encode(entries.length), ...entries]); sections.push(Buffer.from([7]), encode(body.length), body);
    }
    offset = end;
  }
  return new WebAssembly.Module(Buffer.concat(sections));
}

test("strict old/incomplete interfaces work and enabled invalid configuration fails closed", async () => {
  const exports = ["stream_start_policy", "stream_start_repository_policy"];
  for (const removed of [exports, [exports[0]], [exports[1]]]) {
    const old = withoutExports(removed), imported = host(fixtures[0], {}, old);
    assert.deepEqual(Buffer.from(await (await discover(imported)).arrayBuffer()), Buffer.from(await (await discover(host(fixtures[0]))).arrayBuffer()));
    assert.equal((await discover(host(fixtures[0], { wantPolicy: "all-visible-reachable" }, old))).status, removed.includes(exports[1]) ? 503 : 200);
    for (const layout of ["flat", "tree"]) {
      const worker = createWorker(old), capture = await worker.fetch(new Request("https://test.invalid/"), { SNAPSHOT_LAYOUT: layout });
      assert.equal(capture.status, 200); const url = (await capture.json()).clone_url;
      const ordinary = await worker.fetch(new Request(`${url}/info/refs?service=git-upload-pack`)); assert.equal(ordinary.status, 200); await ordinary.arrayBuffer();
      assert.equal((await worker.fetch(new Request(`${url}/info/refs?service=git-upload-pack`), { GIT_WANT_POLICY: "all-visible-reachable" })).status, 503);
    }
  }
  for (const value of ["all", "", null, {}, 1]) assert.equal((await discover(host(fixtures[0], { wantPolicy: value }))).status, 503);
  for (const value of [-1, 1.5, NaN, 2 ** 32]) assert.equal((await discover(host(fixtures[0], { wantPolicy: "all-visible-reachable", maxWantLines: value }))).status, 503);
  assert.equal((await discover(host(fixtures[0], { maxWantLines: -1 }))).status, 200, "maxWantLines is an opt-in cap");
});

test("flat ABI and metadata deployment grant also reset across strict starts and cancellation", async () => {
  const exports = new WebAssembly.Instance(module).exports;
  const parts = ["{}\n", "{}\n", "{\"public\":true}\n", "{\"format\":1}\n"];
  const policy = { GIT_WANT_POLICY: "all-visible-reachable" };
  const worker = createWorker(module);
  for (const layout of ["flat", "tree"]) {
    const capture = await worker.fetch(new Request("https://test.invalid/"), { SNAPSHOT_LAYOUT: layout });
    const url = (await capture.json()).clone_url;
    const ordinary = await worker.fetch(new Request(`${url}/info/refs?service=git-upload-pack`));
    const granted = await worker.fetch(new Request(`${url}/info/refs?service=git-upload-pack`), policy);
    assert.ok(!(await ordinary.text()).includes("allow-reachable-sha1-in-want"));
    assert.ok((await granted.text()).includes("allow-reachable-sha1-in-want"));
  }
  function frame(target, body = Buffer.alloc(0)) {
    const files = parts.map(part => Buffer.from(part)), path = Buffer.from(target), headers = Buffer.from("application/x-git-upload-pack-request");
    const sizes = Buffer.alloc(16); files.forEach((file, index) => sizes.writeUInt32LE(file.length, 4 * index));
    const targetSize = Buffer.alloc(4); targetSize.writeUInt32LE(path.length);
    const lengths = Buffer.alloc(8); lengths.writeUInt32LE(headers.length, 4);
    const input = Buffer.concat([sizes, ...files, targetSize, path, lengths, headers, body]);
    new Uint8Array(exports.memory.buffer, exports.input_ptr(), input.length).set(input); return input.length;
  }
  let length = frame("info/refs?service=git-upload-pack");
  assert.equal(exports.stream_start_policy(length, 0, 0, 1, 64), 3); assert.ok(consume(exports).includes(Buffer.from("allow-reachable-sha1-in-want")));
  // Flat files have ordinary canonical blob IDs; the policy is never part of
  // their snapshot frame, stored metadata, URL or object identity.
  const blob = object("sha1", "blob", parts[0]);
  length = frame("git-upload-pack", fetchBody("sha1", 0, [blob.id]));
  assert.equal(exports.stream_start_policy(length, 1, 0, 1, 1), 3); assert.ok(consume(exports).length > 0);
  length = frame("git-upload-pack", fetchBody("sha1", 0, [blob.id]));
  assert.equal(exports.stream_start(length, 1, 0), -2); assert.equal(exports.stream_read(1), -2);
  length = frame("info/refs?service=git-upload-pack");
  assert.equal(exports.stream_start_policy(length, 0, 0, 1, 1), 3);
  assert.equal(exports.stream_start_policy(length, 0, 0, 9, 1), -2); assert.equal(exports.stream_read(1), -2);
  length = frame("info/refs?service=git-upload-pack");
  assert.equal(exports.stream_start(length, 0, 0), 3); assert.ok(!consume(exports).includes(Buffer.from("allow-reachable-sha1-in-want")));
});

test("a moving projection revokes wants while its retained immutable generation stays isolated", async () => {
  const a = fixtures[0], format = a.format;
  const blob = object(format, "blob", "new authorized generation\n");
  const tree = object(format, "tree", Buffer.concat([Buffer.from("100644 public\0"), Buffer.from(blob.id, "hex")]));
  const commit = object(format, "commit", `tree ${tree.id}\nauthor A <a@example.invalid> 0 +0000\ncommitter A <a@example.invalid> 0 +0000\n\nB\n`);
  const b = { format, objects: [commit, tree, blob], refs: [{ name: "refs/heads/main", id: commit.id }], head: { type: "symbolic", target: "refs/heads/main", id: commit.id } };
  let live = a;
  const worker = createRepositoryWorker(module, async request => {
    const url = new URL(request.url), [, generation, ...path] = url.pathname.split("/");
    return { repository: generation === "A" ? a : live, target: path.join("/") + url.search, wantPolicy: "all-visible-reachable" };
  });
  const missing = a.objects.find(value => value.kind === "blob" && value.body.length === 98304);
  const post = generation => new Request(`https://test.invalid/${generation}/git-upload-pack`, {
    method: "POST", headers: { "content-type": "application/x-git-upload-pack-request" }, body: fetchBody(format, 0, [missing.id]),
  });
  assert.equal((await worker.fetch(new Request("https://test.invalid/live/info/refs?service=git-upload-pack"))).status, 200);
  const retained = await worker.fetch(post("A")); assert.equal(retained.status, 200);
  live = b;
  assert.equal((await worker.fetch(post("live"))).status, 400, "old discovery is no authorization token");
  const concurrent = await worker.fetch(post("A")); assert.equal(concurrent.status, 200); await concurrent.body.cancel();
  assert.ok((await retained.arrayBuffer()).byteLength > missing.body.length);
  const fresh = await worker.fetch(post("A")); assert.equal(fresh.status, 200); await fresh.arrayBuffer();
});
