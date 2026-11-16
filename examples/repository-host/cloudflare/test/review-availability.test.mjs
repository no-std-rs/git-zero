import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import { createRepositoryHost } from "../src/host.mjs";
import { ByteBudget } from "../src/resources.mjs";
import { streamResponse } from "../src/response.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, packet, pack, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const options = { codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "" };
function fixture(extra = {}) {
  const factory = createRepositoryHost({ ...options, ...extra }), db = new DatabaseSync(":memory:"), objects = new Map();
  const env = { K: GRANT_KEY, B: { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); } } };
  const host = new factory.Repository(sqliteContext(db), env);
  const call = (route, method = "GET", body, level = "read") => host.fetch(new Request(`http://host/repo.git/${route}`, { method, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", level), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, ...(body === undefined ? {} : { body, duplex: "half" }) }));
  const push = async (updates, data) => { const response = await call("git-receive-pack", "POST", receive(0, updates, data), "write"); assert.equal(response.status, 200); assert.doesNotMatch(receiveReport(await response.arrayBuffer()), /ng /); };
  return { host, env, db, factory, call, push, async create() { assert.equal((await call("api/v1/repository", "PUT", "{}", "admin")).status, 201); }, async close() { await host.queue; await host.browseQueue; db.close(); } };
}

test("trickling upload-pack and JSON bodies hit a total deadline and release the repository", { timeout: 5000 }, async () => {
  const f = fixture({ requestBodyTotalMs: 60 });
  try {
    await f.create();
    for (const route of ["git-upload-pack", "git-receive-pack", "api/v1/policy"]) {
      let cancelled = false;
      const incoming = receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: project(0).root.id.toString("hex") }], project(0).pack);
      let offset = incoming.indexOf(Buffer.from("PACK")), initial = true;
      const body = new ReadableStream({ async pull(c) {
        await delay(15); if (cancelled) return;
        if (route === "git-receive-pack") {
          if (initial) { initial = false; c.enqueue(incoming.subarray(0, offset)); }
          else c.enqueue(incoming.subarray(offset, ++offset));
        } else c.enqueue(Buffer.from(route.startsWith("git-") ? "0000" : " "));
      }, cancel() { cancelled = true; } });
      const before = Date.now(), uploading = f.call(route, route.startsWith("git-") ? "POST" : "PATCH", body, "admin");
      await delay(20); const queued = f.call("api/v1/refs");
      const response = await uploading; assert.equal(response.status, route === "git-receive-pack" ? 200 : 400);
      const message = route === "git-receive-pack" ? receiveReport(await response.arrayBuffer()) : await response.text(); assert.match(message, /total deadline/);
      assert.equal((await queued).status, 200); assert.ok(Date.now() - before < 1000); assert.equal(cancelled, true);
    }
  } finally { await f.close(); }
});

test("immutable commit, tree and blob browse run during a fetch; publication completes", { timeout: 5000 }, async () => {
  const f = fixture(), data = project(0), tip = data.root.id.toString("hex"), zero = "0".repeat(40); let held;
  try {
    await f.create(); await f.push([{ name: "refs/heads/main", old: zero, new: tip }], data.pack);
    held = await f.call("git-upload-pack", "POST", Buffer.concat([packet(`want ${tip} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]));
    assert.equal(held.status, 200);
    for (const route of ["commit", "tree", "blob?path=file"]) {
      const response = await Promise.race([f.call(`api/v1/${route}`), delay(1000).then(() => { throw new Error(`browse ${route} waited behind fetch`); })]);
      assert.equal(response.status, 200); await response.arrayBuffer();
    }
    let published = false;
    const publication = f.push([{ name: "refs/tags/later", old: zero, new: tip }], pack(0, [])).then(() => { published = true; });
    await publication; assert.equal(published, true, "publication is independent of the live fetch");
    await held.arrayBuffer(); held = null; await publication;
    assert.equal(f.host.env.BYTE_BUDGET.used, 0);
  } finally { await held?.body.cancel().catch(() => {}); await f.close(); }
});

test("minimum throughput permits a slow active stream for unlimited windows, but stops trickling", async () => {
  async function* active() { for (let i = 0; i < 30; i++) { await delay(5); yield new Uint8Array(1024); } }
  const passing = streamResponse(active(), "application/octet-stream", 1000, undefined, 0, { minimumBytesPerSecond: 10000, throughputWindowMs: 40 });
  assert.equal((await passing.response.arrayBuffer()).byteLength, 30720); await passing.done;
  async function* trickle() { for (;;) { await delay(5); yield Uint8Array.of(1); } }
  const failing = streamResponse(trickle(), "application/octet-stream", 1000, undefined, 0, { minimumBytesPerSecond: 10000, throughputWindowMs: 40 });
  await assert.rejects(failing.response.arrayBuffer(), /minimum throughput/); await failing.done;
});

test("byte reservations are FIFO across namespaces and bounded until actual cleanup", async () => {
  const budget = new ByteBudget(100, 1000, 10), order = []; let preempted = false;
  const releaseA = await budget.acquire("a", 60, () => { preempted = true; });
  const waitingB = budget.acquire("b", 60, () => {}).then((release) => { order.push("b"); return release; });
  const waitingC = budget.acquire("c", 20, () => {}).then((release) => { order.push("c"); return release; });
  await delay(20); assert.equal(preempted, true); assert.equal(budget.used, 60); assert.deepEqual(order, []);
  releaseA(); const [releaseB, releaseC] = await Promise.all([waitingB, waitingC]);
  assert.deepEqual(order, ["b", "c"]); assert.equal(budget.used, 80); assert.ok(budget.peak <= 100);
  assert.equal(budget.owners.size, 2); releaseB(); releaseC(); assert.equal(budget.used, 0);
});

test("missing jurisdiction binding fails closed for entry and direct stub", async () => {
  const f = fixture({ jurisdiction: "eu" });
  try {
    const req = () => new Request("http://host/repo.git/api/v1/refs", { headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", "read") } });
    assert.equal((await f.factory.fetch(req(), f.env)).status, 404);
    const direct = new f.factory.Repository({ ...sqliteContext(f.db), id: { toString: () => "eu:repo" } }, f.env);
    assert.equal((await direct.fetch(req())).status, 404);
    assert.equal(f.db.prepare("SELECT COUNT(*) n FROM sqlite_master WHERE type='table'").get().n, 0);
  } finally { await f.close(); }
});

test("refusal draining releases the job and caps concurrent transport tails", { timeout: 5000 }, async () => {
  const f = fixture(); const controllers = [];
  const body = () => new ReadableStream({ start(c) { controllers.push(c); }, cancel() {} });
  try {
    await f.create();
    const first = f.call("unknown", "POST", body(), "admin");
    while (!f.host.env.DRAIN_STATE?.active) await delay(1);
    const second = f.call("unknown", "POST", body(), "admin");
    while (f.host.env.DRAIN_STATE.active < 2) await delay(1);
    const fast = await Promise.race([f.call("api/v1/refs"), delay(1000).then(() => { throw new Error("drain retained repository queue"); })]);
    assert.equal(fast.status, 200); assert.equal(f.host.pendingRequests, 0);
    const third = await f.call("unknown", "POST", body(), "admin"); assert.equal(third.status, 404); assert.equal(f.host.env.DRAIN_STATE.active, 2);
    controllers[0].close(); controllers[1].close();
    assert.equal((await first).status, 404); assert.equal((await second).status, 404); assert.equal(f.host.env.DRAIN_STATE.active, 0);
  } finally { for (const c of controllers) try { c.close(); } catch {} await f.close(); }
});

test("independent incarnations sharing key, namespace and generation reject each other's proofs and cursors", async () => {
  const a = fixture(), b = fixture(), data = project(0), oid = data.root.id.toString("hex");
  try {
    for (const f of [a, b]) { await f.create(); await f.push(["refs/heads/main", "refs/tags/one"].map((name) => ({ name, old: "0".repeat(40), new: oid })), data.pack); }
    const commit = await (await a.call("api/v1/commit")).json();
    const cursor = (await (await a.call("api/v1/refs?limit=1")).json()).nextCursor; assert.ok(cursor);
    const journal = (await (await a.call("api/v1/changes?limit=1")).json()).nextCursor; assert.ok(journal);
    assert.notEqual(a.host.store.exec("SELECT epoch FROM repository_security")[0].epoch, b.host.store.exec("SELECT epoch FROM repository_security")[0].epoch);
    assert.equal((await b.call(`api/v1/commit?rev=${oid}&proof=${commit.proof}`)).status, 400);
    assert.equal((await b.call(`api/v1/refs?cursor=${cursor}`)).status, 400);
    assert.equal((await b.call(`api/v1/changes?cursor=${journal}`)).status, 400);
  } finally { await a.close(); await b.close(); }
});
