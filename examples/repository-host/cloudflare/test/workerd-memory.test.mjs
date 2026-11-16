import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { randomBytes } from "node:crypto";
import { startStandalone } from "./standalone.mjs";
import { inspectorPort, inspectIsolate } from "./inspector.mjs";
import { object, entry, pack, packet, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";

test("workerd memory: two heavy fetches, 30 cached readers and a codec admission burst", { timeout: 120000 }, async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "git-zero-memory-")); let worker, inspector; const held = [];
  try {
    const port = await inspectorPort(); worker = await startStandalone(dir, { authMode: "gateway", grantKey: GRANT_KEY, packCache: true, inspectorPort: port }); inspector = await inspectIsolate(port);
    const usage = () => inspector.command("Runtime.getHeapUsage"), measured = { baseline: await usage() };
    const blob = object(0, 3, randomBytes(16 * 1048576));
    const small = object(0, 3, randomBytes(1048576));
    const browsingTree = object(0, 2, Buffer.concat([Buffer.from("100644 data.bin\0"), blob.id, Buffer.from("100644 run.py\0"), small.id]));
    const commit = object(0, 1, `tree ${browsingTree.id.toString("hex")}\nauthor A <a@example.test> 0 +0000\ncommitter A <a@example.test> 0 +0000\n\nframework\n`);
    const tip = commit.id.toString("hex"), packed = pack(0, [blob, small, browsingTree, commit].map((item) => entry(item.kind, item.body)));
    const call = (ns, route, method = "GET", body, signal) => fetch(`${worker.origin}/${ns}.git/${route}`, { method, signal, headers: { "x-git-zero-grant": mint(ns, "admin"), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, body });
    const state = async (ns) => (await fetch(`${worker.origin}/_test/${ns}/state`, { headers: { "x-git-zero-grant": mint(ns, "admin") } })).json();
    for (const ns of ["a", "b", "cached"]) {
      const r = await call(ns, "api/v1/repository", "PUT", "{}"); assert.equal(r.status, 201);
      const pushed = await call(ns, "git-receive-pack", "POST", receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: tip }], packed)); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    }
    measured.routes = {};
    for (const [route, envelopeMiB] of [["commit", 14], ["history", 19], ["tree", 17], ["blob?path=run.py", 14]]) {
      const before = await usage();
      const response = await call("cached", `api/v1/${route}`, "GET", undefined, AbortSignal.timeout(5000)); if (response.status !== 200) assert.fail(await response.text());
      assert.ok((await response.arrayBuffer()).byteLength > 0);

      measured.routes[route] = { envelopeMiB, before, after: await usage() };
    }
    const controls = (shallow = false) => Buffer.concat([packet(`want ${tip} side-band-64k\n`), ...(shallow ? [packet("deepen 1\n")] : []), Buffer.from("0000"), packet("done\n")]);
    const warm = await call("cached", "git-upload-pack", "POST", controls()); assert.equal(warm.status, 200); await warm.arrayBuffer();
    for (const ns of ["a", "b"]) { const r = await call(ns, "git-upload-pack", "POST", controls(true)); if (r.status !== 200) assert.fail(await r.text()); held.push(r); }
    assert.ok((await state("a")).jobs.active >= 2); assert.ok((await state("b")).jobs.active >= 2);
    measured.twoHeavy = await usage();
    held.push(...await Promise.all(Array.from({ length: 30 }, () => call("cached", "git-upload-pack", "POST", controls()))));
    for (const response of held) assert.equal(response.status, 200);
    assert.equal((await state("cached")).jobs.cachedReaders, 30);
    measured.thirtyCached = await usage();
    // Cache hits still parse controls with a codec. Hundreds of admissions
    // must reuse the four resident arenas, without needing a GC per request.
    for (let round = 0; round < 5; round++) { await Promise.all(Array.from({ length: 20 }, async () => {
      const controller = new AbortController(); let reader;
      try { const response = await call("cached", "git-upload-pack", "POST", controls(), controller.signal); assert.equal(response.status, 200); reader = response.body.getReader(); assert.ok((await reader.read()).value.length); }
      finally { controller.abort(); reader?.cancel().catch(() => {}); }
    })); }
    const deadline = Date.now() + 5000;
    while ((await state("cached")).jobs.cachedReaders !== 30 && Date.now() < deadline) await delay(10);
    measured.afterBurst = await usage(); measured.ledger = (await state("cached")).jobs;
    await inspector.command("HeapProfiler.collectGarbage"); measured.afterGC = await usage();
    assert.ok((await state("a")).jobs.active >= 2); assert.ok((await state("b")).jobs.active >= 2); assert.equal((await state("cached")).jobs.cachedReaders, 30);
    for (const phase of ["twoHeavy", "thirtyCached", "afterBurst", "afterGC"]) {
      const m = measured[phase]; assert.ok(m.usedSize + m.backingStorageSize + m.embedderHeapUsedSize + measured.ledger.wasmLinearBytes < 96 * 1048576, JSON.stringify(measured));
    }
    assert.ok(measured.ledger.memoryPeak <= 64 * 1048576); assert.ok(measured.ledger.codecArenas <= 4);
    t.diagnostic(JSON.stringify(measured));
  } finally { for (const response of held) response.body.cancel().catch(() => {}); inspector?.close(); await worker?.stop(); await rm(dir, { recursive: true, force: true }); }
});
