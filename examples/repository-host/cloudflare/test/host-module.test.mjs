import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import { createRepositoryHost, ByteBudget } from "../src/host.mjs";
import { ObjectSource } from "../src/source.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, receiveReport, packet } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const options = { codec, bindings: { repositories: "CUSTOM_REPOS", objects: "SHARED_DATA", grantKey: "PRODUCT_KEY" }, authMode: "gateway", routePrefix: "/private/git", keyPrefix: "product/git/" };
async function holdFetch(f, name) {
  const data = project(0), tip = data.root.id.toString("hex");
  const pushed = await f.repo(name).fetch(f.request(name, "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: tip }], data.pack) }));
  assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
  return f.repo(name).fetch(f.request(name, "git-upload-pack", "read", { method: "POST", headers: { "content-type": "application/x-git-upload-pack-request" }, body: Buffer.concat([packet(`want ${tip} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]) }));
}
function fixture(extra = {}) {
  const host = createRepositoryHost({ ...options, ...extra }), objects = new Map(), databases = [], instances = new Map();
  const env = { PRODUCT_KEY: GRANT_KEY, SHARED_DATA: { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); }, async list({ prefix, limit }) { const keys = [...objects.keys()].filter((key) => key.startsWith(prefix)).sort(); return { objects: keys.slice(0, limit).map((key) => ({ key })), truncated: keys.length > limit }; } } };
  env.CUSTOM_REPOS = { idFromName: (name) => ({ toString: () => name }), get(id) { const name = id.toString(); if (!instances.has(name)) { const db = new DatabaseSync(":memory:"); databases.push(db); instances.set(name, new host.Repository({ ...sqliteContext(db), id }, env)); } return instances.get(name); } };
  const request = (ns, route, level = "read", init = {}, expiry) => new Request(`http://host/private/git/${ns}.git/${route}`, { ...init, headers: { "x-git-zero-namespace": ns, "x-git-zero-grant": mint(ns, level, expiry ? { expiry } : {}), ...init.headers } });
  return { host, env, objects, instances, request, repo: (ns) => env.CUSTOM_REPOS.get(env.CUSTOM_REPOS.idFromName(ns)), close: () => databases.forEach((db) => db.close()) };
}
test("factory requires explicit deployment identity and validates resource budgets", () => {
  for (const extra of [{ codec: undefined }, { bindings: {} }, { authMode: undefined }, { routePrefix: undefined }, { keyPrefix: undefined }, { keyPrefix: "shared" }, { memory: { canonicalCacheBytes: 25 * 1024 * 1024 } }, { memory: { chunkBytes: 4096 } }, { codecLimits: { maxPackBytes: 2 ** 40 } }, { readLimits: { pageSize: 0 } }]) assert.throws(() => createRepositoryHost({ ...options, ...extra }));
  assert.doesNotThrow(() => createRepositoryHost({ ...options, memory: { cacheEntries: 0, canonicalCaptureBytes: 0 }, readLimits: { pageSize: 1 } }));
});

test("multiple host classes can share a smaller byte pool budgeted for a Rust wrapper", async () => {
  const budget = new ByteBudget(64 * 1024 * 1024), profile = {
    packCacheEnabled: false, memoryBudget: budget, memory: { canonicalCacheBytes: 0, representationCacheBytes: 0, chunkCacheBytes: 0, canonicalCaptureBytes: 65536, representationCaptureBytes: 65536, chunkBytes: 65536 },
    codecLimits: { maxControlBytes: 1024 * 1024 }, readLimits: { metadataBytes: 1024 * 1024, jsonBytes: 128 * 1024 }, readPinBytes: 1024 * 1024,
  };
  const a = fixture(profile), b = fixture(profile); const held = [];
  try {
    for (const [f, name] of [[a, "a"], [b, "b"]]) {
      assert.equal((await f.repo(name).fetch(f.request(name, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
      assert.equal(f.repo(name).env.JOB_MEMORY_BYTES, 13 * 1024 * 1024);
      held.push(await holdFetch(f, name));
    }
    assert.equal(budget.used, 26 * 1024 * 1024); assert.equal(budget.owners.size, 2);
    for (const response of held) await response.arrayBuffer(); held.length = 0;
    for (const f of [a, b]) for (const repo of f.instances.values()) await repo.queue;
    assert.equal(budget.used, 0);
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); a.close(); b.close(); }
});

test("direct stub, renamed bindings, lowered caches and shared-prefix deletion are independent of the entry handler", async () => {
  const f = fixture({ memory: { canonicalCacheBytes: 65536, representationCacheBytes: 65536, chunkCacheBytes: 65536, cacheEntries: 2, canonicalCaptureBytes: 0, representationCaptureBytes: 0, chunkBytes: 65536, metadataEntries: 0 }, readLimits: { pageSize: 1 } });
  const repo = f.repo("repo"), { request } = f;
  try {
    const created = await repo.fetch(request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" })); assert.equal(created.status, 201);
    const fixture = project(0), tip = fixture.root.id.toString("hex"), zero = "0".repeat(40);
    const pushed = await repo.fetch(request("repo", "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(0, ["refs/heads/main", "refs/tags/one"].map((name) => ({ name, old: zero, new: tip })), fixture.pack) })); assert.match(receiveReport(await pushed.arrayBuffer()), /ok refs\/heads\/main/);
    assert.ok([...f.objects.keys()].every((key) => key.startsWith("product/git/repositories/repo/")));
    const refs = await (await repo.fetch(request("repo", "api/v1/refs"))).json(); assert.equal(refs.refs.length, 1); assert.ok(refs.nextCursor, "lower page bound also changes the default");
    const source = new ObjectSource(repo.store, {}, "inspect"); assert.equal(source.canonical.limit, 65536); assert.equal(source.chunks.maxEntries, 2); assert.equal(source.resources.canonicalCaptureBytes, 0);
    source.rememberTemporary("key", {}); assert.equal(source.temporary.size, 0); assert.equal(repo.store.edgeStages.size, 0);
    const blob = await repo.fetch(request("repo", "api/v1/blob?path=file")); assert.equal(await blob.text(), "file contents\n");
    assert.equal((await repo.fetch(request("other", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 404, "a valid other-namespace grant cannot address the wrong object ID");
    const noHeader = request("repo", "api/v1/refs"); noHeader.headers.delete("x-git-zero-namespace"); assert.equal((await repo.fetch(noHeader)).status, 404);
    assert.equal((await f.host.fetch(new Request("http://host/health"), f.env)).status, 404);
    assert.equal((await repo.fetch(new Request("http://host/_test/repo/state", { headers: request("repo", "").headers }))).status, 404);
    const viaHandler = await f.host.fetch(request("repo", "api/v1/commit"), f.env); assert.equal(viaHandler.status, 200); assert.equal((await viaHandler.json()).oid, tip);
    const changedPrefix = createRepositoryHost({ ...options, keyPrefix: "changed/" });
    assert.equal((await new changedPrefix.Repository(repo.ctx, f.env).fetch(request("repo", "api/v1/refs"))).status, 409, "configuration cannot redirect an existing repository into another bucket prefix");
    const foreign = ["foreign/object", "product/git-neighbor/object", "product/git/repositories/another/object", "product/git/repositories/repo-neighbor/object"];
    for (const key of foreign) f.objects.set(key, Uint8Array.of(9)); f.objects.set("product/git/repositories/repo/tmp/orphan", Uint8Array.of(3));
    let deleted; do { const response = await repo.fetch(request("repo", "api/v1/repository", "admin", { method: "DELETE" })); assert.ok([200, 202].includes(response.status)); deleted = await response.json(); } while (!deleted.complete);
    for (const key of foreign) assert.deepEqual(f.objects.get(key), Uint8Array.of(9));
    assert.equal([...f.objects.keys()].some((key) => key.startsWith("product/git/repositories/repo/")), false);
    assert.ok(f.objects.has("product/git/repository-tombstones/v1/repo"));
    await repo.queue;
  } finally { f.close(); }
});

test("stalled repositories cannot starve a co-tenant; admission and expiry are per repository", { timeout: 15000 }, async () => {
  const f = fixture({ maxRequestsPerRepository: 2, maxConcurrentJobs: 1 }); const held = [];
  try {
    for (const name of ["a", "b", "c"]) assert.equal((await f.repo(name).fetch(f.request(name, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    for (const name of ["a", "b"]) held.push(await f.repo(name).fetch(f.request(name, "info/refs?service=git-upload-pack")));
    const expiry = Math.floor(Date.now() / 1000) + 2;
    const pending = [f.repo("a").fetch(f.request("a", "info/refs?service=git-upload-pack", "read", {}, expiry)), f.repo("b").fetch(f.request("b", "info/refs?service=git-upload-pack"))];
    while (f.repo("a").pendingRequests < 2 || f.repo("b").pendingRequests < 2) await delay(1);
    for (const name of ["a", "b"]) {
      const overload = await f.repo(name).fetch(f.request(name, "api/v1/refs")); assert.equal(overload.status, 503); assert.equal(overload.headers.get("retry-after"), "1");
    }
    const other = await Promise.race([f.repo("c").fetch(f.request("c", "api/v1/refs")), delay(1000).then(() => { throw new Error("co-tenant blocked by another repository's readers"); })]);
    assert.equal(other.status, 200); assert.deepEqual((await other.json()).refs, []);
    await delay(Math.max(0, expiry * 1000 - Date.now()) + 30);
    assert.equal((await f.repo("a").fetch(f.request("a", "api/v1/refs", "read", {}, expiry))).status, 404);
    for (const response of held) await response.arrayBuffer(); held.length = 0;
    for (const response of await Promise.all(pending)) { assert.equal(response.status, 200, "admitted grant survives its own repository queue"); await response.arrayBuffer(); }
    for (const repo of f.instances.values()) await repo.queue;
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); f.close(); }
});

test("token defaults preserve an uncapped queue and independent repository progress", { timeout: 10000 }, async () => {
  const f = fixture({ authMode: "token", bindings: { ...options.bindings, token: "TOKEN" } }); f.env.TOKEN = "public-test-token";
  const request = (ns, route) => f.request(ns, route, "read", { headers: { authorization: "Bearer public-test-token" } });
  let held;
  try {
    held = await f.repo("a").fetch(request("a", "info/refs?service=git-upload-pack"));
    const pending = Array.from({ length: 100 }, () => f.repo("a").fetch(request("a", "info/refs?service=git-upload-pack")).then(async (response) => { await response.arrayBuffer(); return response; }));
    // Requests may already finish concurrently; all 100 must succeed under the default admission policy.
    assert.equal((await f.repo("b").fetch(request("b", "api/v1/refs"))).status, 404, "missing read still completes independently");
    await held.arrayBuffer(); held = null;
    assert.ok((await Promise.all(pending)).every((response) => response.status === 200));
    assert.equal(f.repo("a").env.RESPONSE_IDLE_MS, 60000); assert.equal(f.repo("a").env.RESPONSE_TOTAL_MS, 0);
    assert.equal(f.repo("a").env.MEMORY.canonicalCacheBytes, 24 * 1024 * 1024);
    for (const repo of f.instances.values()) await repo.queue;
  } finally { await held?.body.cancel().catch(() => {}); f.close(); }
});

test("contended byte budget preempts a reader and admits a co-tenant without exceeding reserved memory", { timeout: 5000 }, async () => {
  const f = fixture({ packCacheEnabled: false, isolateMemoryBytes: 80 * 1024 * 1024, memoryContentionMs: 30, memoryWaitMs: 2000, responseIdleMs: 10000 }), held = [];
  try {
    for (const name of ["a", "b", "c"]) assert.equal((await f.repo(name).fetch(f.request(name, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    for (const name of ["a", "b"]) held.push(await holdFetch(f, name));
    const budget = f.repo("a").env.BYTE_BUDGET;
    assert.equal(budget.used, 2 * f.repo("a").env.JOB_MEMORY_BYTES); assert.equal(budget.owners.size, 2);
    const started = Date.now(), next = await holdFetch(f, "c");
    assert.equal(next.status, 200); assert.ok(Date.now() - started < 1500); await next.arrayBuffer();
    const results = await Promise.allSettled(held.map((response) => response.arrayBuffer())); held.length = 0;
    assert.ok(results.some((result) => result.status === "rejected" && /memory contention/.test(result.reason.message)));
    for (const repo of f.instances.values()) await repo.queue;
    assert.ok(budget.peak <= 96 * 1024 * 1024); assert.equal(budget.used, 0); assert.equal(budget.owners.size, 0);
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); for (const repo of f.instances.values()) await repo.queue; f.close(); }
});

test("a stalled reader times out and releases only its repository's scratch", { timeout: 3000 }, async () => {
  const f = fixture({ responseIdleMs: 30 }); let held;
  try {
    assert.equal((await f.repo("a").fetch(f.request("a", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    held = await f.repo("a").fetch(f.request("a", "info/refs?service=git-upload-pack"));
    const pending = f.repo("a").fetch(f.request("a", "info/refs?service=git-upload-pack"));
    const next = await pending; assert.equal(next.status, 200); await next.arrayBuffer();
    await delay(40); await assert.rejects(held.arrayBuffer(), /response reader idle/); held = null;
    assert.equal(f.repo("a").env.RESPONSE_TOTAL_MS, 0);
    assert.equal(f.repo("a").env.RESPONSE_MINIMUM_RATE, 16384);
    assert.equal(f.repo("a").env.MEMORY.canonicalCacheBytes, 2 * 1024 * 1024);
    await f.repo("a").queue;
  } finally { await held?.body.cancel().catch(() => {}); f.close(); }
});


test("stream cleanup remains attached to the Durable Object lifetime until leases retire", async () => {
  const f = fixture(), tasks = []; let held;
  try {
    const repo = f.repo("repo"); repo.ctx.waitUntil = (promise) => { tasks.push(promise); };
    assert.equal((await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    held = await repo.fetch(f.request("repo", "info/refs?service=git-upload-pack"));
    let retired = false; tasks.at(-1).then(() => { retired = true; }); await delay(1);
    assert.ok(tasks.length >= 2); assert.equal(retired, false); assert.ok(repo.env.BYTE_BUDGET.used > 0);
    await held.arrayBuffer(); held = null; await Promise.all(tasks);
    assert.equal(retired, true); assert.equal(repo.env.BYTE_BUDGET.used, 0);
  } finally { await held?.body.cancel().catch(() => {}); await Promise.allSettled(tasks); f.close(); }
});


test("token byte admission preserves more than 1024 FIFO waiters while gateway admission remains bounded", async () => {
  for (const unlimited of [true, false]) {
    const budget = new ByteBudget(1, 5000, 5000), releaseFirst = await budget.acquire("held", 1, () => {});
    const count = unlimited ? 1100 : 1024;
    const pending = Array.from({ length: count }, (_, i) => budget.acquire(`repo-${i}`, 1, () => {}, unlimited ? { waitMs: 0 } : {}).then((release) => ({ release }), (error) => ({ error })));
    try {
      assert.equal(budget.waiters.length, count);
      if (!unlimited) await assert.rejects(budget.acquire("overflow", 1, () => {}), /admission budget/);
      releaseFirst();
      for (const item of pending) { const result = await item; assert.ifError(result.error); result.release(); }
      assert.equal(budget.used, 0); assert.equal(budget.peak, 1);
    } finally { releaseFirst(); for (const item of pending) (await item).release?.(); }
  }
});
