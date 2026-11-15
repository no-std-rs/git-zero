import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import { createRepositoryHost } from "../src/host.mjs";
import { ObjectSource } from "../src/source.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const options = { codec, bindings: { repositories: "CUSTOM_REPOS", objects: "SHARED_DATA", grantKey: "PRODUCT_KEY" }, authMode: "gateway", routePrefix: "/private/git", keyPrefix: "product/git/" };
function fixture(extra = {}) {
  const host = createRepositoryHost({ ...options, ...extra }), objects = new Map(), databases = [], instances = new Map();
  const env = { PRODUCT_KEY: GRANT_KEY, SHARED_DATA: { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); }, async list({ prefix, limit }) { const keys = [...objects.keys()].filter((key) => key.startsWith(prefix)).sort(); return { objects: keys.slice(0, limit).map((key) => ({ key })), truncated: keys.length > limit }; } } };
  env.CUSTOM_REPOS = { idFromName: (name) => ({ toString: () => name }), get(id) { const name = id.toString(); if (!instances.has(name)) { const db = new DatabaseSync(":memory:"); databases.push(db); instances.set(name, new host.Repository({ ...sqliteContext(db), id }, env)); } return instances.get(name); } };
  const request = (ns, route, level = "read", init = {}, expiry) => new Request(`http://host/private/git/${ns}.git/${route}`, { ...init, headers: { "x-git-zero-namespace": ns, "x-git-zero-grant": mint(ns, level, expiry ? { expiry } : {}), ...init.headers } });
  return { host, env, objects, instances, request, repo: (ns) => env.CUSTOM_REPOS.get(env.CUSTOM_REPOS.idFromName(ns)), close: () => databases.forEach((db) => db.close()) };
}
test("factory requires explicit deployment identity and only lowers resource budgets", () => {
  for (const extra of [{ codec: undefined }, { bindings: {} }, { authMode: undefined }, { routePrefix: undefined }, { keyPrefix: undefined }, { keyPrefix: "shared" }, { memory: { canonicalCacheBytes: 25 * 1024 * 1024 } }, { memory: { chunkBytes: 4096 } }, { codecLimits: { maxPackBytes: 2 ** 40 } }, { readLimits: { pageSize: 0 } }]) assert.throws(() => createRepositoryHost({ ...options, ...extra }));
  assert.doesNotThrow(() => createRepositoryHost({ ...options, memory: { cacheEntries: 0, canonicalCaptureBytes: 0 }, readLimits: { pageSize: 1 } }));
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

test("grant admission precedes the shared-isolate gate and overload bounds", { timeout: 15000 }, async () => {
  const f = fixture({ maxActiveJobs: 1, maxPendingJobs: 2, maxRequestsPerRepository: 2 }); let held;
  try {
    for (const name of ["a", "b"]) assert.equal((await f.repo(name).fetch(f.request(name, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    held = await f.repo("a").fetch(f.request("a", "info/refs?service=git-upload-pack"));
    const expiry = Math.floor(Date.now() / 1000) + 2;
    const pending = f.repo("b").fetch(f.request("b", "api/v1/refs", "read", {}, expiry));
    while (!f.repo("b").pendingRequests) await delay(1);
    const overload = await f.repo("b").fetch(f.request("b", "api/v1/refs")); assert.equal(overload.status, 503); assert.equal(overload.headers.get("retry-after"), "1");
    await delay(Math.max(0, expiry * 1000 - Date.now()) + 30);
    assert.equal((await f.repo("b").fetch(f.request("b", "api/v1/refs", "read", {}, expiry))).status, 404, "expired arrivals are refused even while the gate is full");
    await held.arrayBuffer(); held = null;
    const response = await pending; assert.equal(response.status, 200); assert.deepEqual((await response.json()).refs, []);
    for (const repo of f.instances.values()) await repo.queue;
  } finally { await held?.body.cancel().catch(() => {}); f.close(); }
});
