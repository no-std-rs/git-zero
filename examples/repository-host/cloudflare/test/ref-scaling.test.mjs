import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import { Readable } from "node:stream";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { mint } from "./grant-fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { performance } from "node:perf_hooks";
import { fixture, holdFetch } from "./review4-fixture.mjs";
import { jobStore, snapshot, pruneRefVersions } from "../src/jobs.mjs";
import { Proofs } from "../src/proofs.mjs";
import { object, entry, pack, receiveReport, packet } from "./fixture.mjs";

test("real Git clones 5000 tags; indexed wants/include-tag and historical intervals stay bounded", { timeout: 15000 }, async (t) => {
  const f = fixture({ packCacheEnabled: false }), repo = f.repo("scale"), historical = new DatabaseSync(":memory:"), root = await mkdtemp(join(tmpdir(), "git-zero-ref-scale-")); let server;
  const git = async (args) => {
    const child = spawn("git", args, { cwd: root, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" }, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "", stderr = ""; child.stdout.on("data", (b) => { stdout += b; }); child.stderr.on("data", (b) => { stderr += b; });
    const [code] = await once(child, "exit"); assert.equal(code, 0, stderr); return stdout;
  };
  try {
    assert.equal((await repo.fetch(f.request("scale", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    await (await holdFetch(f, "scale")).arrayBuffer(); await repo.queue;
    const store = repo.store, tip = store.exec("SELECT oid FROM refs")[0].oid;
    const tags = Array.from({ length: 5000 }, (_, i) => object(0, 4, `object ${tip}\ntype commit\ntag tag-${i}\ntagger A <a@example.test> 0 +0000\n\ntag ${i}\n`));
    const updates = tags.map((tag, i) => ({ name: `refs/tags/tag-${i}`, old: "0".repeat(40), new: tag.id.toString("hex") }));
    const pushed = await repo.fetch(f.request("scale", "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(0, updates, pack(0, tags.map((tag) => entry(4, tag.body)))) }));
    assert.equal(pushed.status, 200); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /); await repo.queue;
    const state = snapshot(store), owner = { id: crypto.randomUUID(), abort: new AbortController(), snapshot: state }, scoped = jobStore(store, owner, { read: true });
    let started = performance.now();
    for (const tag of tags) assert.equal(scoped.isTip(tag.id.toString("hex")), true);
    const elapsed = performance.now() - started; assert.ok(elapsed < 1000, `5000 distinct indexed tip checks took ${elapsed}ms`);
    const plan = store.exec("EXPLAIN QUERY PLAN SELECT 1 FROM ref_versions WHERE oid=? AND last_generation IS NULL AND first_generation<=? LIMIT 1", tip, state.generation);
    assert.match(JSON.stringify(plan), /ref_versions_oid/); assert.doesNotMatch(JSON.stringify(plan), /SCAN/);
    const controls = Buffer.concat(tags.map((tag, i) => packet(`want ${tag.id.toString("hex")}${i ? "" : " side-band-64k include-tag"}\n`)).concat([Buffer.from("0000"), packet("done\n")]));
    let yielded = false; setTimeout(() => { yielded = true; }, 0);
    const response = await repo.fetch(f.request("scale", "git-upload-pack", "read", { method: "POST", headers: { "content-type": "application/x-git-upload-pack-request" }, body: controls }));
    assert.equal(response.status, 200); assert.ok((await response.arrayBuffer()).byteLength > 0); assert.equal(yielded, true); await repo.queue;
    server = createServer(async (incoming, outgoing) => {
      try {
        const result = await f.host.fetch(new Request(`http://host${incoming.url}`, { method: incoming.method, headers: { ...incoming.headers, "x-git-zero-grant": mint("scale", "read") }, ...(incoming.method === "GET" ? {} : { body: Readable.toWeb(incoming), duplex: "half" }) }), f.env);
        outgoing.writeHead(result.status, Object.fromEntries(result.headers));
        if (result.body) Readable.fromWeb(result.body).on("error", (error) => outgoing.destroy(error)).pipe(outgoing); else outgoing.end();
      } catch (error) { outgoing.destroy(error); }
    });
    server.listen(0, "127.0.0.1"); await once(server, "listening"); started = performance.now();
    await git(["-c", "protocol.version=2", "-c", "gc.auto=0", "clone", "--quiet", "--bare", `http://127.0.0.1:${server.address().port}/private/git/scale.git`, "clone.git"]);
    const cloneMs = performance.now() - started;
    const refs = (await git(["--git-dir=clone.git", "for-each-ref", "--format=%(refname) %(objectname)"])).trim().split("\n");
    assert.deepEqual(refs.sort(), [`refs/heads/main ${tip}`, ...updates.map((update) => `${update.name} ${update.new}`)].sort());
    await git(["--git-dir=clone.git", "fsck", "--strict"]);
    // A separate generated interval model isolates a heavily pushed ref from
    // the real clone's publication log. Every old oid is a Git-verified tag.
    historical.exec("CREATE TABLE ref_versions(name BLOB,first_generation INTEGER,last_generation INTEGER,oid TEXT,peeled TEXT,PRIMARY KEY(name,first_generation)) WITHOUT ROWID");
    for (const row of store.exec("SELECT sql FROM sqlite_master WHERE type='index' AND name IN ('ref_versions_oid','ref_versions_peeled','ref_versions_retired')")) historical.exec(row.sql);
    const insert = historical.prepare("INSERT INTO ref_versions VALUES(?,?,?,?,?)"), name = Buffer.from("refs/tags/heavily-pushed");
    historical.exec("BEGIN");
    for (let i = 0; i < 10000; i++) insert.run(name, i, i + 1, tags[i % tags.length].id.toString("hex"), tip);
    insert.run(name, 10000, null, tags[0].id.toString("hex"), tip); historical.exec("COMMIT");
    const base = { env: {}, exec: (sql, ...bindings) => historical.prepare(sql).all(...bindings) }, live = jobStore(base, { ...owner, snapshot: { ...state, generation: 10001 } }, { read: true });
    started = performance.now();
    for (let i = 0; i < 5000; i++) assert.equal(live.isTip(tip), true, "peeled lookups skip all 10000 closed intervals");
    const historicalMs = performance.now() - started; assert.ok(historicalMs < 1000, `historical interval tip checks took ${historicalMs}ms`);
    assert.equal(pruneRefVersions(base, 5, 3), 3);
    const older = jobStore(base, { ...owner, snapshot: { ...state, generation: 5 } }, { read: true }); assert.equal(older.isTip(tags[5].id.toString("hex")), true);
    assert.equal(base.exec("SELECT COUNT(*) n FROM ref_versions WHERE last_generation>5")[0].n, 9995);
    scoped.exec("SELECT oid FROM refs LIMIT 1"); scoped.exec("SELECT oid FROM refs LIMIT 1"); assert.equal(scoped.sqlMetrics.compilations, 1); assert.equal(scoped.sqlMetrics.hits, 1);
    t.diagnostic(JSON.stringify({ distinctTips: 5000, tipLookupMs: elapsed, cloneMs, oldVersions: 10000, peeledLookupMs: historicalMs }));
  } finally { server?.closeAllConnections(); if (server) await new Promise((resolve) => server.close(resolve)); await repo.queue; historical.close(); f.close(); await rm(root, { recursive: true, force: true }); }
});

test("proof payload fields cannot override the repository binding", async () => {
  const f = fixture(), repo = f.repo("repo");
  try {
    await repo.fetch(f.request("repo", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    const proofs = new Proofs(repo.store, 0), token = await proofs.sign("object", { v: 4, op: "changes", namespace: "other", epoch: "old", generation: 999, oid: "a".repeat(40) });
    const value = await proofs.verify("object", token);
    assert.equal(value.v, 3); assert.equal(value.namespace, "repo"); assert.equal(value.generation, 0); assert.equal(value.op, "object"); assert.equal(value.epoch, proofs.epoch);
  } finally { await repo.queue; f.close(); }
});
