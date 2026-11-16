import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, packet, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { FetchJob } from "../src/fetch.mjs";
import { fixture as deployment, holdFetch } from "./review4-fixture.mjs";
import { setTimeout as delay } from "node:timers/promises";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
test("16 cached fetches share one generation and release all codecs before client EOF", { timeout: 10000 }, async () => {
  const factory = createRepositoryHost({ codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "" });
  const db = new DatabaseSync(":memory:"), objects = new Map(), host = new factory.Repository(sqliteContext(db), { K: GRANT_KEY, B: {
    async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; },
    async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); },
  } });
  const call = (route, method = "GET", body, role = "read") => host.fetch(new Request(`http://host/repo.git/${route}`, { method, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", role), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, ...(body === undefined ? {} : { body }) }));
  const held = [], fixture = project(0), tip = fixture.root.id.toString("hex");
  try {
    assert.equal((await call("api/v1/repository", "PUT", "{}", "admin")).status, 201);
    const pushed = await call("git-receive-pack", "POST", receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: tip }], fixture.pack), "write"); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    const body = Buffer.concat([packet(`want ${tip} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]);
    held.push(...await Promise.all(Array.from({ length: 16 }, () => call("git-upload-pack", "POST", body))));
    for (const response of held) { if (response.status !== 200) assert.fail(await response.text()); }
    const cache = host.packCache; assert.equal(cache.metrics.builds, 1); assert.equal(host.env.BYTE_BUDGET.used, host.env.BYTE_BUDGET.residentBytes + 16 * host.env.CACHE_READER_MEMORY_BYTES);
    assert.equal(host.store.fetchActive.size, 0);
    for (const { name } of db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB 'job_*_fetch_*'").all()) assert.equal(db.prepare(`SELECT COUNT(*) n FROM ${name}`).get().n, 0);
    const payloads = await Promise.all(held.map((response) => response.arrayBuffer())); held.length = 0;
    for (const value of payloads) assert.deepEqual(value, payloads[0]);
    await host.queue; assert.equal(host.env.BYTE_BUDGET.used, host.env.BYTE_BUDGET.residentBytes); assert.equal(cache.pins.size, 0); assert.equal(cache.metrics.served, 16);
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); await host.queue; db.close(); }
});

test("cancelling the initiating client before preparation does not cancel its shared builder or followers", { timeout: 5000 }, async () => {
  const f = deployment(), repo = f.repo("repo"); let unblock;
  try {
    await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    // Seed without warming the pack cache.
    repo.env.PACK_CACHE_ENABLED = false; await (await holdFetch(f, "repo")).arrayBuffer(); await repo.queue; repo.env.PACK_CACHE_ENABLED = true;
    const barrier = new Promise((resolve) => { unblock = resolve; }), resources = repo.resources.bind(repo);
    repo.resources = (owner, ns) => { resources(owner, ns); if (owner.internal) { const reserve = owner.reserve; owner.reserve = async (...args) => { await barrier; return reserve(...args); }; } };
    const first = holdFetch(f, "repo");
    while (![...repo.jobs.values()].some((job) => job.internal)) await delay(1);
    const client = [...repo.jobs.values()].find((job) => !job.internal);
    const followers = Array.from({ length: 4 }, () => holdFetch(f, "repo"));
    client.abort.abort(new Error("first client disconnected before preparation")); unblock();
    const cancelled = await first; assert.notEqual(cancelled.status, 200);
    const results = await Promise.all(followers); const payloads = [];
    for (const response of results) { assert.equal(response.status, 200, await response.clone().text()); payloads.push(await response.arrayBuffer()); }
    assert.ok(payloads[0].byteLength > 0); for (const value of payloads) assert.deepEqual(value, payloads[0]);
    await repo.queue; assert.equal(repo.packCache.metrics.builds, 1); assert.equal(repo.store.fetchActive.size, 0);
  } finally { unblock?.(); await repo.queue; f.close(); }
});

test("a failed pre-header builder lets its followers generate independently", { timeout: 5000 }, async () => {
  const f = deployment(), repo = f.repo("repo");
  try {
    await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    repo.env.PACK_CACHE_ENABLED = false; await (await holdFetch(f, "repo")).arrayBuffer(); await repo.queue; repo.env.PACK_CACHE_ENABLED = true;
    repo.buildShared = (entry, cache) => { setTimeout(() => cache.fail(entry, new Error("unrelated builder admission failure")), 20); };
    await Promise.all(Array.from({ length: 8 }, async () => { const response = await holdFetch(f, "repo"); assert.equal(response.status, 200, await response.clone().text()); assert.ok((await response.arrayBuffer()).byteLength > 0); }));
    await repo.queue; assert.equal(repo.store.fetchActive.size, 0);
  } finally { await repo.queue; f.close(); }
});

test("shared pack construction spans hundreds of chunks beyond its initial deadline while making progress", { timeout: 10000 }, async (t) => {
  const f = deployment({ packCacheBuildMs: 100, packCacheMinimumBytesPerSecond: 1024 }), repo = f.repo("repo");
  try {
    await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    const { PackCache } = await import("../src/pack-cache.mjs"), cache = new PackCache(repo.store, repo.env), entry = cache.begin("progress-test");
    const put = repo.store.put.bind(repo.store); let puts = 0, cleaned = 0;
    repo.store.put = async (...args) => { puts++; await delay(2); return put(...args); };
    const count = 256, part = new Uint8Array(repo.env.PACK_CACHE_CHUNK_BYTES).fill(37);
    assert.equal(cache.claim(entry, 0, count * part.length), true);
    const started = performance.now();
    await cache.produce(entry, { async *response() { for (let i = 0; i < count; i++) yield part; }, async cleanup() { cleaned++; } }, new AbortController().signal);
    const elapsedMs = performance.now() - started;
    assert.ok(elapsedMs > 100); assert.equal(entry.complete, true); assert.equal(puts, count); assert.equal(cleaned, 1);
    let bytes = 0; for await (const data of cache.read(entry, new AbortController().signal)) { assert.ok(data.every((value) => value === 37)); bytes += data.length; }
    assert.equal(bytes, count * part.length); t.diagnostic(JSON.stringify({ chunks: count, bytes, elapsedMs, initialGraceMs: 100 }));
  } finally { await repo.queue; f.close(); }
});

test("builder cleanup errors still retire its codec lease, gate and lifetime", { timeout: 5000 }, async () => {
  const f = deployment(), repo = f.repo("repo"), cleanup = FetchJob.prototype.cleanup;
  try {
    await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    repo.env.PACK_CACHE_ENABLED = false; await (await holdFetch(f, "repo")).arrayBuffer(); await repo.queue; repo.env.PACK_CACHE_ENABLED = true;
    FetchJob.prototype.cleanup = async function () { await cleanup.call(this); if (this.store.env.JOB_OWNER?.internal) throw new Error("injected builder cleanup failure"); };
    await Promise.all(Array.from({ length: 4 }, async () => { const response = await holdFetch(f, "repo"); assert.equal(response.status, 200); assert.ok((await response.arrayBuffer()).byteLength > 0); }));
    await repo.queue;
    assert.equal(repo.env.BYTE_BUDGET.used, repo.env.BYTE_BUDGET.residentBytes); assert.equal(repo.env.BYTE_BUDGET.owners.size, 0);
    assert.equal(repo.gate.active, 0); assert.equal(repo.readerGate.active, 0); assert.equal(repo.jobs.size, 0);
  } finally { FetchJob.prototype.cleanup = cleanup; await repo.queue; f.close(); }
});
