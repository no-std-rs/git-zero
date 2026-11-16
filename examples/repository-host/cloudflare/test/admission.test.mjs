import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { ByteBudget } from "../src/resources.mjs";
import { fixture, holdFetch } from "./review4-fixture.mjs";
import { packet, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const MiB = 1048576;

test("namespace-fair byte admission leaves room for browse, never preempts a clone, and reuses four arenas", { timeout: 10000 }, async (t) => {
  const f = fixture({ packCacheEnabled: false }), held = [];
  try {
    for (const ns of ["busy", "light"]) {
      await f.repo(ns).fetch(f.request(ns, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
      await (await holdFetch(f, ns)).arrayBuffer(); await f.repo(ns).queue;
    }
    held.push(...await Promise.all([holdFetch(f, "busy"), holdFetch(f, "busy")]));
    const budget = f.repo("busy").env.BYTE_BUDGET, times = [];
    for (let round = 0; round < 10; round++) {
      const before = performance.now();
      const requests = Array.from({ length: 30 }, () => f.repo("busy").fetch(f.request("busy", "api/v1/commit")).then(async (r) => { assert.equal(r.status, 200, await r.clone().text()); return r.json(); }));
      const light = await Promise.race([f.repo("light").fetch(f.request("light", "api/v1/commit")), delay(1000).then(() => { throw new Error("light tenant starved"); })]);
      assert.equal(light.status, 200); await light.json(); times.push(performance.now() - before); await Promise.all(requests);
    }
    for (const response of held) assert.ok((await response.arrayBuffer()).byteLength > 0); held.length = 0;
    for (const repo of f.instances.values()) await repo.queue;
    assert.ok(budget.peak <= 64 * MiB); assert.ok(budget.arenas.length <= 4); assert.equal(budget.used, budget.residentBytes); assert.equal(budget.owners.size, 0);
    t.diagnostic(JSON.stringify({ rounds: 10, browseReads: 300, lightMaximumMs: Math.max(...times), bytePeak: budget.peak, retainedCodecBytes: budget.residentBytes }));
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); for (const repo of f.instances.values()) await repo.queue; f.close(); }
});

test("a namespace cannot spend co-tenant headroom on sustained push leases; round robin skips an oversized waiter", async () => {
  const budget = new ByteBudget(64 * MiB), options = { codec: true, heavy: true, fair: true }, held = [];
  try {
    for (let i = 0; i < 3; i++) held.push(await budget.acquire("pushes", 14 * MiB, null, options));
    const controller = new AbortController();
    const pending = budget.acquire("pushes", 14 * MiB, null, { ...options, signal: controller.signal }).catch((e) => e);
    const before = performance.now(), read = await budget.acquire("other", 18 * MiB, null, { ...options, heavy: false });
    assert.ok(performance.now() - before < 50); read(); assert.equal(budget.owners.get("pushes").size, 3);
    controller.abort(); assert.match((await pending).message, /cancelled/);
  } finally { held.forEach((release) => release()); }
});

test("an incumbent cannot replenish heavy leases ahead of a larger co-tenant waiter", async () => {
  const budget = new ByteBudget(64 * MiB), options = { codec: true, heavy: true, fair: true }, held = [];
  for (let i = 0; i < 3; i++) held.push(await budget.acquire("busy", 14 * MiB, null, options));
  let admitted = false;
  const larger = budget.acquire("other", 22 * MiB, null, options).then((release) => { admitted = true; return release; });
  const replenishing = budget.acquire("busy", 14 * MiB, null, options);
  held.shift()(); await delay(1); assert.equal(admitted, false);
  assert.equal(budget.owners.get("busy").size, 2, "new busy work must not consume bytes needed by the larger waiter");
  held.shift()(); const release = await larger; assert.equal(admitted, true); release();
  (await replenishing)(); held.shift()();
  assert.equal(budget.used, budget.residentBytes);
});

test("bounded cache-control phases can admit behind two heavy fetches and thirty cached streams", async () => {
  const budget = new ByteBudget(64 * MiB), held = [], options = { codec: true, heavy: true, fair: true };
  try {
    held.push(await budget.acquire("a", 14 * MiB, null, options), await budget.acquire("b", 14 * MiB, null, options));
    for (let i = 0; i < 30; i++) held.push(await budget.acquire("cached", 192 * 1024, null, { fair: true }));
    for (let i = 0; i < 100; i++) { const lease = await budget.acquire("cached", 11 * MiB, null, { ...options, headroom: false }); lease(); }
    assert.ok(budget.peak <= budget.limit); assert.equal(budget.owners.get("a").size, 1); assert.equal(budget.owners.get("b").size, 1);
  } finally { held.forEach((release) => release()); }
});

test("blob streams use small reservations but cannot spend the metadata arena", { timeout: 5000 }, async () => {
  const f = fixture(), held = [];
  try {
    for (const ns of ["blobs", "reader"]) {
      await f.repo(ns).fetch(f.request(ns, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
      await (await holdFetch(f, ns)).arrayBuffer(); await f.repo(ns).queue;
    }
    held.push(...await Promise.all(Array.from({ length: 3 }, () => f.repo("blobs").fetch(f.request("blobs", "api/v1/blob?path=file")))));
    const pool = f.repo("blobs").env.BYTE_BUDGET;
    assert.equal([...pool.owners.get("blobs")].filter((lease) => lease.heavy && lease.arena).length, 3);
    const read = await Promise.race([f.repo("reader").fetch(f.request("reader", "api/v1/history")), delay(1000).then(() => { throw new Error("blob streams consumed metadata capacity"); })]);
    assert.equal(read.status, 200); await read.json();
    for (const response of held) assert.equal(await response.text(), "file contents\n"); held.length = 0;
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); for (const repo of f.instances.values()) await repo.queue; f.close(); }
});

test("pack throughput replaces the absolute body deadline and admission is outside prefix time", { timeout: 5000 }, async () => {
  const f = fixture({ requestBodyTotalMs: 50, requestBodyMinimumBytesPerSecond: 100, requestBodyThroughputWindowMs: 1000 });
  try {
    const repo = f.repo("slow"); await repo.fetch(f.request("slow", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    await (await holdFetch(f, "slow")).arrayBuffer(); await repo.queue;
    const tip = repo.store.exec("SELECT oid FROM refs")[0].oid;
    const packed = receive(0, [{ name: "refs/tags/slow", old: "0".repeat(40), new: tip }], Buffer.from("5041434b0000000200000000029d08823bd8a8eab510ad6ac75c823cfd3ed31e", "hex"));
    // Prefix is prompt. Pack data takes >4 times the configured former total.
    const prefix = packed.indexOf(Buffer.from("PACK")); let at = 0;
    const body = new ReadableStream({ async pull(controller) { if (at === packed.length) { controller.close(); return; } if (at >= prefix) await delay(60); const end = at < prefix ? prefix : Math.min(at + 8, packed.length); controller.enqueue(packed.subarray(at, end)); at = end; } }, { highWaterMark: 0 });
    const before = performance.now(), result = await repo.fetch(f.request("slow", "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body, duplex: "half" }));
    assert.ok(performance.now() - before > 200); assert.equal(result.status, 200); assert.match(receiveReport(await result.arrayBuffer()), /ok refs\/tags\/slow/);
    assert.ok(packet("done\n").length);
  } finally { for (const repo of f.instances.values()) await repo.queue; f.close(); }
});

test("neither a repository gate wait nor the prefix-to-pack memory wait spends the body allowance", { timeout: 5000 }, async () => {
  const f = fixture({ requestBodyTotalMs: 40, maxConcurrentJobs: 1 }); let held;
  try {
    const repo = f.repo("wait"); await repo.fetch(f.request("wait", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    // Seed and drain once, then hold only the gate with a copied advertisement.
    await (await holdFetch(f, "wait")).arrayBuffer(); await repo.queue;
    const tip = repo.store.exec("SELECT oid FROM refs")[0].oid;
    const resources = repo.resources.bind(repo);
    repo.resources = (owner, ns) => { resources(owner, ns); const reserve = owner.reserve; owner.reserve = async (...args) => { if (owner.controlBytes) await delay(80); return reserve(...args); }; };
    held = await repo.fetch(f.request("wait", "info/refs?service=git-upload-pack"));
    const pending = repo.fetch(f.request("wait", "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(0, [{ name: "refs/tags/after-waits", old: "0".repeat(40), new: tip }], Buffer.from("5041434b0000000200000000029d08823bd8a8eab510ad6ac75c823cfd3ed31e", "hex")) }));
    await delay(100); await held.arrayBuffer(); held = null;
    const response = await pending; assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs\/tags\/after-waits/);
  } finally { await held?.body.cancel().catch(() => {}); for (const repo of f.instances.values()) await repo.queue; f.close(); }
});
