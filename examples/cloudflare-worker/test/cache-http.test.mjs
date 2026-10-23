import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { createPackCache, decodePackRecord, encodePackRecord } from "../src/pack-cache.mjs";
import { createRepositoryWorker } from "../src/repository.mjs";
import { cache, deferred, handler, httpRequest, module, requestBody, unframe } from "./cache-fixture.mjs";
import { withServer } from "./http.mjs";
import { directory, execute, fixtures, gitBinary, gitEnvironment } from "./repository-fixture.mjs";

for (const repository of fixtures) {
  for (const kind of ["kv", "r2"]) {
    for (const version of [0, 1, 2]) {
      test(`${kind} real Git ${repository.format} v${version} cold/warm clones compare refs, objects, checkout and fsck`, () => checkRealGitCachedClones(repository, kind, version));
    }
  }
}

test("simultaneous cold HTTP requests await one publication and retain independent response cursors", async () => {
  for (const kind of ["kv", "r2"]) {
    const { binding, policy } = cache(kind);
    const repository = fixtures[0];
    const worker = handler(repository, policy);
    binding.putGate = deferred();
    let completed = 0;
    const pending = Array.from({ length: 6 }, () => worker.fetch(httpRequest(repository)).then((response) => { completed++; return response; }));
    await until(() => binding.operations.some((value) => value.operation === "put"));
    assert.equal(completed, 0);
    assert.equal(policy.statistics().misses, 1);
    assert.equal(policy.statistics().writes, 0);
    binding.putGate.resolve();
    const responses = await Promise.all(pending);
    assert.ok(responses.every((response) => response.status === 200));
    await responses[0].body.cancel();
    const bodies = await Promise.all(responses.slice(1).map((response) => response.arrayBuffer()));
    const digest = (bytes) => createHash("sha256").update(unframe(Buffer.from(bytes))).digest("hex");
    assert.ok(bodies.every((body) => digest(body) === digest(bodies[0])));
    assert.equal(policy.statistics().writes, 1);
    assert.equal(policy.statistics().hits, 5);
    assert.equal(policy.statistics().inflight, 0);
    assert.equal(policy.statistics().waiters, 0);
  }
});

test("snapshot A to B to A responses remain immutable across awaits, eviction and cache recreation", async () => {
  for (const kind of ["kv", "r2"]) {
    const repository = fixtures[0];
    const other = { ...repository, head: { type: "detached", id: repository.refs.find((ref) => ref.name === "refs/heads/side").id } };
    const { binding, policy } = cache(kind);
    let selected = repository;
    const worker = createRepositoryWorker(module, async () => ({ repository: selected, target: "git-upload-pack", packCache: policy }));
    binding.getGate = deferred();
    const a = worker.fetch(httpRequest(repository));
    await until(() => binding.operations.length === 1);
    selected = other;
    const b = worker.fetch(httpRequest(other));
    await until(() => binding.operations.length === 2);
    binding.getGate.resolve();
    const firstA = await bytes(await a);
    const firstB = await bytes(await b);
    assert.notEqual(digest(firstA), digest(firstB));
    selected = repository;
    const hitA = await bytes(await worker.fetch(httpRequest(repository)));
    assert.equal(digest(firstA), digest(hitA));
    const recreated = createPackCache({ backend: kind, binding, namespace: "test-private" });
    const restarted = handler(repository, recreated);
    assert.equal(digest(await bytes(await restarted.fetch(httpRequest(repository)))), digest(firstA));
    assert.equal(recreated.statistics().hits, 1);
    binding.records.clear();
    assert.equal(digest(await bytes(await restarted.fetch(httpRequest(repository)))), digest(firstA));
    assert.equal(recreated.statistics().misses, 1);
  }
});

test("host repository and policy mutation during storage await cannot alter a prepared request", async () => {
  const original = fixtures[0];
  const repository = { ...original, objects: original.objects.map((value) => ({ ...value, body: Buffer.from(value.body) })),
    refs: original.refs.map((value) => ({ ...value })), head: { ...original.head } };
  const { binding, policy } = cache();
  const selected = { repository, target: "git-upload-pack", packCache: policy, wantPolicy: "all-visible-reachable", maxWantLines: 64 };
  const worker = createRepositoryWorker(module, async () => selected);
  binding.getGate = deferred();
  const pending = worker.fetch(httpRequest(original));
  await until(() => binding.operations.length === 1);
  for (const object of repository.objects) object.body.fill(0);
  repository.refs.length = 0;
  selected.wantPolicy = "invalid";
  selected.maxWantLines = 0;
  selected.target = "HEAD";
  binding.getGate.resolve();
  const response = await pending;
  assert.equal(response.status, 200);
  const control = handler(original, null);
  assert.equal(digest(await bytes(response)), digest(await bytes(await control.fetch(httpRequest(original)))));
});

test("revoked wants and duplicate-counting limits fail before warmed cache access", async () => {
  const repository = fixtures[0];
  const blob = repository.objects.find((value) => value.kind === "blob");
  const { binding, policy } = cache();
  let grant = "all-visible-reachable", limit = 64;
  const worker = createRepositoryWorker(module, async () => ({ repository, target: "git-upload-pack", packCache: policy, wantPolicy: grant, maxWantLines: limit }));
  const request = () => httpRequest(repository, { wants: [blob.id] });
  assert.equal((await worker.fetch(request())).status, 200);
  const operations = binding.operations.length;
  grant = undefined;
  assert.equal((await worker.fetch(request())).status, 400);
  assert.equal(binding.operations.length, operations);
  grant = "all-visible-reachable";
  limit = 1;
  const denied = await worker.fetch(httpRequest(repository, { wants: [blob.id, blob.id] }));
  assert.equal(denied.status, 413);
  assert.equal(binding.operations.length, operations);
});

test("corrupt hits, wrong authenticated object sets, backend failures and failed puts expose no Git success", async () => {
  for (const kind of ["kv", "r2"]) {
    const repository = fixtures[0];
    const { binding, policy } = cache(kind);
    const worker = handler(repository, policy);
    assert.equal((await worker.fetch(httpRequest(repository))).status, 200);
    const [name, record] = [...binding.records][0];
    const altered = record.slice();
    altered[70] ^= 1;
    binding.records.set(name, altered);
    let response = await worker.fetch(httpRequest(repository));
    assert.equal(response.status, 502);
    assert.equal(response.headers.get("Cache-Control"), "no-store");
    assert.ok(!(await response.text()).includes("PACK"));
    binding.records.set(name, record);
    const filterRequest = () => httpRequest(repository, { extra: ["filter blob:none"] });
    assert.equal((await worker.fetch(filterRequest())).status, 200);
    const filtered = [...binding.records].find(([key]) => key !== name);
    const selectedKey = name.split("/").at(-1);
    const wrong = await decodePackRecord(filtered[0].split("/").at(-1), filtered[1], 147456, 0);
    binding.records.set(name, await encodePackRecord(selectedKey, wrong, Math.floor(Date.now() / 1000) + 3600));
    response = await worker.fetch(httpRequest(repository));
    assert.equal(response.status, 502, "valid envelope still requires exact canonical pack closure");
    binding.records.clear();
    binding.readError = new Error("offline");
    assert.equal((await worker.fetch(httpRequest(repository))).status, 503);
    binding.readError = null;
    binding.writeError = new Error("storage full");
    assert.equal((await worker.fetch(httpRequest(repository))).status, 503);
    assert.equal(binding.records.size, 0);
    assert.equal(policy.statistics().inflight, 0);
    binding.writeError = null;
    assert.equal((await worker.fetch(httpRequest(repository))).status, 200);
  }
});

test("request cancellation during read, miss publication and cold-miss waiting does not poison retries", async () => {
  for (const stage of ["get", "put"]) {
    const repository = fixtures[0];
    const { binding, policy } = cache();
    const worker = handler(repository, policy);
    const gate = deferred();
    binding[`${stage}Gate`] = gate;
    const controller = new AbortController();
    const pending = worker.fetch(httpRequest(repository, { signal: controller.signal }));
    await until(() => binding.operations.some((value) => value.operation === stage));
    controller.abort();
    const cancelled = await pending;
    assert.equal(cancelled.status, 499);
    assert.equal(policy.statistics().inflight, 0);
    gate.resolve();
    binding[`${stage}Gate`] = null;
    const response = await worker.fetch(httpRequest(repository));
    assert.equal(response.status, 200);
    assert.ok(unframe(await bytes(response)).length > 0);
  }
});

test("cached shallow and filtered responses retain each request's own control and exact pack", async () => {
  for (const repository of fixtures) {
    const { policy } = cache();
    const worker = handler(repository, policy, { revisionLimits: { maxExclusions: 8, graphReadLimit: 1048576 } });
    const direct = handler(repository, null, { revisionLimits: { maxExclusions: 8, graphReadLimit: 1048576 } });
    for (const version of [0, 1, 2]) {
      for (const extra of [["deepen 1"], ["filter blob:none"], ["deepen 2", "filter blob:limit=4"], ["deepen-since 1"]]) {
        const options = { version, extra };
        const reference = await direct.fetch(httpRequest(repository, options));
        const first = await worker.fetch(httpRequest(repository, options));
        assert.equal(first.status, reference.status);
        assert.equal(first.status, 200, `${repository.format} v${version} ${extra}`);
        const expected = await bytes(reference);
        const actual = await bytes(first);
        assert.equal(digest(actual), digest(expected));
        assert.equal(control(actual).toString("hex"), control(expected).toString("hex"));
        const hit = await bytes(await worker.fetch(httpRequest(repository, options)));
        assert.equal(Buffer.compare(hit, actual), 0);
      }
    }
    assert.ok(policy.statistics().hits > 0);
  }
});

test("discovery, HEAD, loose objects and invalid routes retain normal responses without storage", async () => {
  const repository = fixtures[0];
  const { binding, policy } = cache();
  const worker = handler(repository, policy);
  const direct = handler(repository, null);
  const blob = repository.objects.find((value) => value.kind === "blob");
  for (const target of ["HEAD", "info/refs?service=git-upload-pack", "info/refs", "missing", `objects/${blob.id.slice(0, 2)}/${blob.id.slice(2)}`]) {
    const request = () => new Request(`http://cache.invalid/repo.git/${target}`);
    const response = await worker.fetch(request());
    const expected = await direct.fetch(request());
    assert.equal(response.status, expected.status);
    assert.equal(response.headers.get("Content-Type"), expected.headers.get("Content-Type"));
    assert.equal(Buffer.compare(await bytes(response), await bytes(expected)), 0);
  }
  assert.equal(binding.operations.length, 0);
});

async function bytes(response) { return Buffer.from(await response.arrayBuffer()); }
function digest(wire) { return createHash("sha256").update(unframe(wire)).digest("hex"); }
function control(wire) {
  let offset = 0;
  while (offset < wire.length) {
    const size = Number.parseInt(wire.subarray(offset, offset + 4).toString(), 16);
    if (wire[offset + 4] === 1 || wire.subarray(offset, offset + 4).toString() === "PACK") break;
    offset += Math.max(size, 4);
  }
  return wire.subarray(0, offset);
}
async function until(predicate) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail("request did not reach expected storage operation");
}

async function verify(git, clone, repository) {
  await git("-C", clone, "fsck", "--strict");
  assert.equal((await git("-C", clone, "rev-parse", "HEAD")).stdout.toString().trim(), repository.head.id);
  const ids = (await git("-C", clone, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)")).stdout.toString().trim().split("\n").sort();
  assert.deepEqual(ids, repository.objects.map((object) => object.id).sort());
  for (const object of repository.objects) {
    assert.equal(Buffer.compare((await git("-C", clone, "cat-file", object.kind, object.id)).stdout, object.body), 0);
  }
  const expected = (await git("-C", repository.source, "ls-tree", "-r", "-z", "HEAD")).stdout;
  assert.equal(Buffer.compare((await git("-C", clone, "ls-tree", "-r", "-z", "HEAD")).stdout, expected), 0);
  for (const record of expected.toString().split("\0").filter(Boolean)) {
    const [metadata, path] = record.split("\t");
    const [mode, , id] = metadata.split(" ");
    if (mode !== "120000") assert.equal(Buffer.compare(await readFile(join(clone, path)), repository.objects.find((object) => object.id === id).body), 0);
  }
}

async function checkRealGitCachedClones(repository, kind, version) {
  const { binding, policy } = cache(kind);
  const worker = handler(repository, policy);
  const git = (...args) => execute(gitBinary, ["-c", `protocol.version=${version}`, ...args], {
    env: gitEnvironment(repository.format), encoding: "buffer", timeout: 60000,
  });
  await withServer((request) => worker.fetch(request), async (origin) => {
    const url = `${origin}/repo.git`;
    const advertised = (await git("ls-remote", "--symref", url)).stdout.toString().trim().split("\n").sort();
    const oracle = (await git("ls-remote", "--symref", repository.source)).stdout.toString().trim().split("\n").sort();
    assert.deepEqual(advertised, oracle);
    assert.equal(binding.operations.length, 0, "discovery/ref commands never touch pack storage");
    const roots = [];
    for (const state of ["cold", "warm"]) {
      const clone = join(directory, `cache-${kind}-${repository.format}-${version}-${state}`);
      await git("clone", "-q", url, clone);
      await verify(git, clone, repository);
      roots.push(clone);
    }
    assert.ok(policy.statistics().misses > 0);
    assert.ok(policy.statistics().hits > 0);
    assert.equal(policy.statistics().writes, policy.statistics().misses);
    const records = binding.records.size;
    await git("-C", roots[0], "fetch", "-q", "origin");
    assert.equal(binding.records.size, records, "up-to-date negotiation does not add cache records");
    binding.records.clear();
    const clone = join(directory, `cache-${kind}-${repository.format}-${version}-evicted`);
    await git("clone", "-q", url, clone);
    await verify(git, clone, repository);
    assert.ok(binding.records.size > 0);
  });
}
