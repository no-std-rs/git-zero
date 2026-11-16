import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { randomBytes } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
const credentials = "Basic " + Buffer.from("owner:workshop-fixture").toString("base64");
async function git(args, { input, allowFailure = false } = {}) {
  const child = spawn("git", ["-c", `http.extraHeader=Authorization: ${credentials}`, ...args], { env: { ...process.env, GIT_TERMINAL_PROMPT: "0" }, stdio: ["pipe", "pipe", "pipe"] });
  let stdout = "", stderr = ""; child.stdout.on("data", (b) => { stdout += b; }); child.stderr.on("data", (b) => { stderr += b; }); child.stdin.end(input);
  const [code] = await once(child, "exit"); if (!allowFailure) assert.equal(code, 0, `${args.join(" ")}\n${stderr}`);
  return { code, stdout: stdout.trim(), stderr };
}
async function gateway(origin, { namespace = "workshop", token, beforePack, packStarted, linkDelayMs = 100 } = {}) {
  let barrier = null;
  const server = createServer(async (incoming, outgoing) => {
    let reader;
    try {
      if (incoming.headers.authorization !== credentials) { incoming.resume(); outgoing.writeHead(401, { "WWW-Authenticate": 'Basic realm="workshop"' }); outgoing.end(); return; }
      if (!incoming.url.startsWith("/team/framework.git/")) { incoming.resume(); outgoing.writeHead(404); outgoing.end(); return; }
      const route = incoming.url.slice("/team/framework.git/".length);
      if (barrier && route === "git-receive-pack") { const b = barrier; if (++b.count === 2) { barrier = null; b.release(); } await b.ready; }
      const headers = new Headers(); for (const field of ["content-type", "content-encoding", "git-protocol"]) if (incoming.headers[field]) headers.set(field, incoming.headers[field]);
      headers.set("x-git-zero-grant", mint(namespace, "admin"));
      if (token) headers.set("authorization", `Bearer ${token}`);
      const result = await fetch(`${origin}/${namespace}.git/${route}`, { method: incoming.method, headers, ...(incoming.method === "GET" ? {} : { body: incoming, duplex: "half" }) });
      outgoing.writeHead(result.status, Object.fromEntries(result.headers));
      if (!result.body) { outgoing.end(); return; }
      reader = result.body.getReader(); outgoing.on("close", () => { if (!outgoing.writableEnded) reader.cancel().catch(() => {}); });
      let detectedPack = false, tail = Buffer.alloc(0);
      for (;;) {
        const part = await reader.read(); if (part.done) break;
        // v2 ls-refs also POSTs to upload-pack: pause only an actual pack,
        // while the host still owns its codec, never the small ref response.
        if (route === "git-upload-pack" && !detectedPack && (packStarted || beforePack)) {
          const prefix = Buffer.concat([tail, part.value]);
          if (prefix.includes(Buffer.from("PACK"))) { detectedPack = true; packStarted?.(); await beforePack?.(); }
          tail = prefix.subarray(Math.max(0, prefix.length - 3));
        }
        for (let at = 0; at < part.value.length; at += 65536) {
          // ~5 Mbit/s per link; concurrent clients have independent links.
          if (route === "git-upload-pack") await delay(linkDelayMs);
          if (outgoing.destroyed) { await reader.cancel(); return; }
          if (!outgoing.write(part.value.subarray(at, at + 65536))) await once(outgoing, "drain");
        }
      }
      outgoing.end();
    } catch (error) { if (!outgoing.headersSent) outgoing.writeHead(502); outgoing.destroy(error); }
    finally { reader?.releaseLock(); }
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  return {
    url: `http://127.0.0.1:${server.address().port}/team/framework.git`,
    race() { let release; const ready = new Promise((r) => { release = r; }); barrier = { count: 0, ready, release }; },
    async close() { server.closeAllConnections(); await new Promise((r) => server.close(r)); },
  };
}
for (const [runtime, boot] of [["Wrangler R2/SQLite", (path) => start(path, { vars: { AUTH_MODE: "gateway", GRANT_KEY, PACK_CACHE_ENABLED: "true" } })], ["direct workerd with S3/SQLite", (path) => startStandalone(path, { authMode: "gateway", grantKey: GRANT_KEY, packCache: true })]]) {
  test(`${runtime}: 16 real clones share a pack while browse, pushes and deletion proceed`, { timeout: 240000 }, async (t) => {
    const dir = await mkdtemp(join(tmpdir(), "git-zero-workshop-")); let worker, proxy;
    try {
      const source = join(dir, "source"); await mkdir(source); await git(["init", "--quiet", "-b", "main", source]);
      await git(["-C", source, "config", "user.name", "Workshop"]); await git(["-C", source, "config", "user.email", "workshop@example.test"]);
      await mkdir(join(source, "python", "framework", "analysis"), { recursive: true });
      await writeFile(join(source, "python", "framework", "analysis", "dataset.bin"), randomBytes(8 * 1024 * 1024));
      await writeFile(join(source, "python", "framework", "analysis", "run.py"), "print('initial')\n");
      await git(["-C", source, "add", "."]); await git(["-C", source, "commit", "--quiet", "-m", "initial framework"]);
      const old = (await git(["-C", source, "rev-parse", "HEAD"])).stdout;
      worker = await boot(join(dir, "state")); proxy = await gateway(worker.origin);
      const call = (route, method = "GET", body) => fetch(`${worker.origin}/${route}`, { method, headers: { "x-git-zero-grant": mint("workshop", "admin"), "content-type": "application/json" }, body });
      const state = async () => { const r = await call("_test/workshop/state"); assert.equal(r.status, 200, await r.clone().text()); return r.json(); };
      const created = await call("workshop.git/api/v1/repository", "PUT", "{}"); assert.equal(created.status, 201, await created.text());
      await git(["-C", source, "push", "--quiet", proxy.url, "main"]);
      const clone = (name, allowFailure = false) => git(["-c", "protocol.version=2", "clone", "--quiet", proxy.url, join(dir, name)], { allowFailure });
      let startTime = performance.now(); await clone("baseline"); const singleMs = performance.now() - startTime;
      assert.equal((await state()).packCache.builds, 1);
      // A new ref generation, with identical objects, forces a genuinely cold build.
      await git(["-C", source, "push", "--quiet", proxy.url, "HEAD:refs/tags/workshop"]);
      const before = (await state()).packCache.builds;
      startTime = performance.now(); let done = 0;
      const clones = Array.from({ length: 16 }, (_, i) => clone(`participant-${i}`).then((r) => { done++; return r; }));
      const deadline = Date.now() + 20000; let live;
      do { await delay(50); live = await state(); } while (live.jobs.cachedReaders < 16 && Date.now() < deadline);
      assert.equal(live.jobs.cachedReaders, 16, "all clones must overlap in the host, with no per-repository stream lock");
      const browseAt = performance.now(), browse = await call("workshop.git/api/v1/tree?path=python/framework/analysis");
      assert.equal(browse.status, 200, await browse.clone().text()); const entries = await browse.json(); assert.ok(entries.entries.some((e) => e.name === "run.py"));
      const browseMs = performance.now() - browseAt; assert.ok(browseMs < 1500, `browse blocked ${browseMs}ms`);
      await writeFile(join(source, "python", "framework", "analysis", "run.py"), "print('published during workshop')\n");
      await git(["-C", source, "commit", "-am", "concurrent publication", "--quiet"]);
      const tip = (await git(["-C", source, "rev-parse", "HEAD"])).stdout;
      const pushAt = performance.now(); await git(["-C", source, "push", "--quiet", proxy.url, "main"]); const pushMs = performance.now() - pushAt;
      assert.equal(done, 0, "publication must finish while clones are still streaming");
      await Promise.all(clones); const sixteenMs = performance.now() - startTime;
      assert.ok(sixteenMs < 3.5 * singleMs, `${sixteenMs}ms for 16 clones vs ${singleMs}ms for one`);
      for (let i = 0; i < 16; i++) { const path = join(dir, `participant-${i}`); assert.equal((await git(["-C", path, "rev-parse", "HEAD"])).stdout, old); await git(["-C", path, "fsck", "--no-dangling"]); }
      const after = await state(); assert.equal(after.packCache.builds, before + 1, "one CPU build for sixteen clients");
      await git(["-C", join(dir, "baseline"), "fetch", "--quiet", "origin"]); assert.equal((await git(["-C", join(dir, "baseline"), "rev-parse", "origin/main"])).stdout, tip);
      const tree = (await git(["-C", source, "rev-parse", "HEAD^{tree}"])).stdout;
      const a = (await git(["-C", source, "commit-tree", tree, "-p", tip], { input: "analyst A\n" })).stdout, b = (await git(["-C", source, "commit-tree", tree, "-p", tip], { input: "analyst B\n" })).stdout;
      const push = (oid, ref, allowFailure = false) => git(["-C", source, "push", "--quiet", proxy.url, `${oid}:refs/heads/${ref}`], { allowFailure });
      await Promise.all([push(a, "analyst-a"), push(b, "analyst-b")]);
      proxy.race(); const raced = await Promise.all([push(a, "race", true), push(b, "race", true)]);
      assert.deepEqual(raced.map((r) => r.code).sort(), [0, 1]); assert.match(raced.find((r) => r.code).stderr, /stale|lock|non.fast.forward|ref changed/i);
      const interrupted = Array.from({ length: 4 }, (_, i) => clone(`deleted-${i}`, true));
      const deleteDeadline = Date.now() + 20000;
      do { await delay(50); live = await state(); } while (live.jobs.cachedReaders < 4 && Date.now() < deleteDeadline);
      assert.ok(live.jobs.cachedReaders >= 4);
      let deleted = await call("workshop.git/api/v1/repository", "DELETE"); assert.equal(deleted.status, 202);
      assert.equal((await call("workshop.git/api/v1/refs")).status, 404);
      const cancelled = await Promise.all(interrupted); assert.ok(cancelled.every((r) => r.code !== 0));
      for (let i = 0; deleted.status === 202 && i < 100; i++) { await deleted.arrayBuffer(); await delay(20); deleted = await call("workshop.git/api/v1/repository", "DELETE"); }
      assert.equal(deleted.status, 200, await deleted.text());
      t.diagnostic(JSON.stringify({ runtime, singleMs: Math.round(singleMs), sixteenMs: Math.round(sixteenMs), ratio: +(sixteenMs / singleMs).toFixed(2), browseMs: Math.round(browseMs), pushMs: Math.round(pushMs), buildsForSixteen: 1, memoryPeak: after.jobs.memoryPeak }));
    } catch (error) { console.error(error.stack); console.error(worker?.logs()); throw error; } finally { await proxy?.close(); await worker?.stop(); await rm(dir, { recursive: true, force: true }); }
  });
}


test("token mode: two repositories clone concurrently with real Git and independent slow links", { timeout: 120000 }, async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "git-zero-token-workshop-")); let worker, a, b;
  try {
    const source = join(dir, "source"); await mkdir(source); await git(["init", "--quiet", "-b", "main", source]);
    await git(["-C", source, "config", "user.name", "Token"]); await git(["-C", source, "config", "user.email", "token@example.test"]);
    await writeFile(join(source, "data.bin"), randomBytes(16 * 1048576)); await git(["-C", source, "add", "."]); await git(["-C", source, "commit", "--quiet", "-m", "token clone"]);
    worker = await startStandalone(join(dir, "state"));
    let release, markA, markB; const blocked = new Promise((r) => { release = r; }), aStarted = new Promise((r) => { markA = r; }), bStarted = new Promise((r) => { markB = r; });
    a = await gateway(worker.origin, { namespace: "token-a", token: TOKEN, linkDelayMs: 50, packStarted: markA, beforePack: () => blocked });
    b = await gateway(worker.origin, { namespace: "token-b", token: TOKEN, linkDelayMs: 50, packStarted: markB });
    await git(["-C", source, "push", "--quiet", a.url, "main"]); await git(["-C", source, "push", "--quiet", b.url, "main"]);
    let done = 0; const started = performance.now();
    const clones = [git(["clone", "--quiet", a.url, join(dir, "clone-0")]).then(() => { done++; })];
    await aStarted;
    clones.push(git(["clone", "--quiet", b.url, join(dir, "clone-1")]).then(() => { done++; }));
    try { await Promise.race([bStarted, delay(5000).then(() => { throw new Error("second token clone could not start while first link is paused"); })]); assert.equal(done, 0); }
    finally { release(); }
    const state = async (ns) => (await fetch(`${worker.origin}/_test/${ns}/state`, { headers: { authorization: `Bearer ${TOKEN}` } })).json();
    let overlapping = false;
    for (let tries = 0; tries < 100 && !done; tries++) { const [aa, bb] = await Promise.all([state("token-a"), state("token-b")]); if (aa.jobs.active >= 2 && bb.jobs.active >= 2) { overlapping = true; break; } await delay(20); }
    assert.equal(overlapping, true, "both token fetch streams must be live at once");
    await Promise.all(clones); assert.equal(done, 2);
    const elapsedMs = performance.now() - started; assert.ok(elapsedMs < 30000, `two 16 MiB clones took ${elapsedMs}ms`);
    for (let i = 0; i < 2; i++) await git(["-C", join(dir, `clone-${i}`), "fsck", "--no-dangling"]);
    t.diagnostic(JSON.stringify({ tokenConcurrentClones: 2, elapsedMs }));
  } finally { await a?.close(); await b?.close(); await worker?.stop(); await rm(dir, { recursive: true, force: true }); }
});
