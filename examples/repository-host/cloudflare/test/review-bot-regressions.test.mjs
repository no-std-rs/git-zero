import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Store, ChunkWriter } from "../src/storage.mjs";
import { jobStore } from "../src/jobs.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { fixture } from "./review4-fixture.mjs";
import { git, receive } from "./browse-fixture.mjs";
import { receiveReport } from "./fixture.mjs";
const owner = () => ({ id: crypto.randomUUID(), abort: new AbortController() });

test("a concurrent chunk claim survives an already-started deletion without blocking other keys", { timeout: 5000 }, async () => {
  const database = new DatabaseSync(":memory:"), objects = new Map(), data = Uint8Array.of(1, 2, 3);
  let entered, resume, paused = false;
  const deleting = new Promise((r) => { entered = r; }), resumed = new Promise((r) => { resume = r; });
  const store = new Store(sqliteContext(database), { BLOBS: {
    async put(key, value) { objects.set(key, value.slice()); },
    async delete(keys) { if (!paused) { paused = true; entered(); await resumed; } for (const key of keys) objects.delete(key); },
  } }, "race");
  try {
    const a = jobStore(store, owner()), b = jobStore(store, owner()), c = jobStore(store, owner());
    const first = store.stage(0), second = store.stage(0), other = store.stage(0);
    const one = new ChunkWriter(a, first); await one.write(data); await one.finish();
    const original = store.exec("SELECT key FROM stage_chunks WHERE stage=?", first)[0].key;
    const cleanup = a.abandon(first); await deleting;
    const two = new ChunkWriter(b, second); await two.write(data); const writing = two.finish();
    // A different key must remain independent of the held DELETE.
    const independent = new ChunkWriter(c, other); await independent.write(Uint8Array.of(8, 9));
    await Promise.race([independent.finish(), delay(500).then(() => { throw new Error("unrelated key was serialized"); })]);
    // Give the competing writer a turn while the storage DELETE is held.
    await Promise.race([writing, delay(25)]); resume(); await Promise.all([cleanup, writing]);
    const row = store.exec("SELECT key FROM stage_chunks WHERE stage=?", second)[0];
    assert.equal(row.key, original, "content-addressed deduplication is retained");
    assert.deepEqual(objects.get(row.key), data, "successful writer must retain its physical chunk");
    assert.equal(store.storageAccounting().knownBytes, 5, "only live physical bytes are accounted");
    await b.abandon(second); await c.abandon(other); assert.equal(objects.size, 0);
    assert.equal(store.chunkOperations?.size ?? 0, 0);
  } finally { resume(); database.close(); }
});

for (const temporary of [false, true]) test(`warm alarms observe job-view recovery deadlines temporary=${temporary}`, async () => {
  const f = fixture(), repo = f.repo("recovery"), now = Date.now; let fail = true;
  try {
    const created = await repo.fetch(f.request("recovery", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }));
    assert.equal(created.status, 201); await repo.queue;
    const remove = f.env.SHARED_DATA.delete.bind(f.env.SHARED_DATA);
    f.env.SHARED_DATA.delete = async (keys) => { if (fail) throw new Error("injected cleanup failure"); return remove(keys); };
    const view = jobStore(repo.store, owner()), stage = view.stage(0);
    const writer = new ChunkWriter(view, stage, temporary ? "canonical" : null, temporary);
    await writer.write(Uint8Array.of(1, 2, 3)); await writer.finish();
    if (temporary) await view.clearTemporary(stage, { defer: true }); else await view.abandon(stage, { defer: true });
    assert.equal(f.objects.size, 1);
    const due = view.recoveryDue; assert.ok(Number.isFinite(due)); fail = false;
    Date.now = () => due + 1; await repo.alarm(); Date.now = now;
    assert.equal(repo.store.exec("SELECT 1 FROM stages WHERE id=?", stage).length, 0, "warm alarm must reclaim the deferred stage");
    assert.equal(repo.store.exec("SELECT 1 FROM temp_chunks").length, 0);
    assert.equal(repo.store.exec("SELECT bytes FROM read_scratch_usage WHERE id=1")[0].bytes, 0);
    assert.equal(f.objects.size, 0); assert.equal(repo.store.storageAccounting().knownBytes, 0);
  } finally { Date.now = now; await repo.queue; f.close(); }
});

for (const format of [0, 1]) test(`tree byte pagination includes proofs and cursor in an 8 KiB budget format=${format}`, { timeout: 10000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-proof-pages-")), directory = join(root, "source");
  const f = fixture({ readLimits: { jsonBytes: 8192 } }), ns = "n".repeat(80), repo = f.repo(ns);
  try {
    await mkdir(directory); git(directory, ["init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`]);
    await Promise.all(Array.from({ length: 20 }, (_, i) => writeFile(join(directory, `file-${String(i).padStart(2, "0")}.py`), "# analysis\n")));
    git(directory, ["add", "."]); git(directory, ["commit", "-m", "proof page fixture"]);
    const tip = git(directory, ["rev-parse", "HEAD"]).toString().trim();
    const create = await repo.fetch(f.request(ns, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }) }));
    assert.equal(create.status, 201);
    const pushed = await repo.fetch(f.request(ns, "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(format, [{ name: "refs/heads/main", old: "0".repeat(format ? 64 : 40), new: tip }], git(directory, ["pack-objects", "--stdout", "--all"])) }));
    assert.equal(pushed.status, 200); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    const names = []; let cursor, pages = 0;
    do {
      const response = await repo.fetch(f.request(ns, `api/v1/tree?${cursor ? `cursor=${cursor}` : "limit=20"}`));
      const body = new Uint8Array(await response.arrayBuffer()); assert.equal(response.status, 200, new TextDecoder().decode(body)); assert.ok(body.length <= 8192);
      const page = JSON.parse(new TextDecoder().decode(body)); assert.ok(page.entries.length > 0);
      if (!pages) {
        assert.ok(page.entries.length < 20); assert.ok(page.nextCursor);
        const first = page.entries[0], blob = await repo.fetch(f.request(ns, `api/v1/blob?oid=${first.oid}&proof=${first.proof}`));
        assert.equal(blob.status, 200); assert.equal(await blob.text(), "# analysis\n");
      }
      names.push(...page.entries.map((entry) => entry.name)); cursor = page.nextCursor; pages++;
      assert.ok(pages <= 20, "pagination must always advance");
    } while (cursor);
    assert.deepEqual(names, git(directory, ["ls-tree", "--name-only", "HEAD"]).toString().trim().split("\n"));
  } finally { await repo.queue; f.close(); await rm(root, { recursive: true, force: true }); }
});

test("SHA-256 history byte pagination budgets the signed cursor for long namespaces", { timeout: 10000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-history-envelope-")), directory = join(root, "source");
  const f = fixture({ readLimits: { jsonBytes: 8192 } }), ns = "h".repeat(80), repo = f.repo(ns);
  try {
    await mkdir(directory); git(directory, ["init", "--initial-branch=main", "--object-format=sha256"]);
    for (let i = 0; i < 4; i++) { await writeFile(join(directory, "analysis.py"), `# revision ${i}\n`); git(directory, ["add", "."]); git(directory, ["commit", "-m", `revision ${i}`]); }
    const expected = git(directory, ["rev-list", "--first-parent", "HEAD"]).toString().trim().split("\n");
    assert.equal((await repo.fetch(f.request(ns, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ objectFormat: "sha256" }) }))).status, 201);
    const pushed = await repo.fetch(f.request(ns, "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(1, [{ name: "refs/heads/main", old: "0".repeat(64), new: expected[0] }], git(directory, ["pack-objects", "--stdout", "--all"])) }));
    assert.equal(pushed.status, 200); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    const referenceResponse = await repo.fetch(f.request(ns, "api/v1/history?limit=3")); assert.equal(referenceResponse.status, 200);
    const referenceBytes = new Uint8Array(await referenceResponse.arrayBuffer()), reference = JSON.parse(new TextDecoder().decode(referenceBytes));
    assert.equal(reference.commits.length, 3); assert.ok(reference.nextCursor);
    // Choose a valid cap exactly where the old 512-byte allowance admitted
    // these three real records. Their actual signed envelope is larger.
    const budget = 512 + reference.commits.reduce((size, entry) => size + Buffer.byteLength(JSON.stringify(entry)), 0);
    assert.ok(referenceBytes.length > budget); assert.ok(budget < 8192);
    repo.env.READ_LIMITS = { ...repo.env.READ_LIMITS, jsonBytes: budget };
    const actual = []; let cursor, pages = 0;
    do {
      const response = await repo.fetch(f.request(ns, `api/v1/history${cursor ? `?cursor=${cursor}` : ""}`));
      const data = new Uint8Array(await response.arrayBuffer()); assert.equal(response.status, 200, new TextDecoder().decode(data)); assert.ok(data.length <= budget);
      const page = JSON.parse(new TextDecoder().decode(data)); assert.ok(page.commits.length > 0);
      if (!pages) { assert.ok(page.commits.length < 3); assert.ok(page.nextCursor); }
      actual.push(...page.commits.map((entry) => entry.oid)); cursor = page.nextCursor; pages++; assert.ok(pages <= 4);
    } while (cursor);
    assert.deepEqual(actual, expected);
  } finally { await repo.queue; f.close(); await rm(root, { recursive: true, force: true }); }
});

for (const temporary of [false, true]) test(`recovery preserves cleanup scheduled by a protected job during R2 deletion temporary=${temporary}`, { timeout: 5000 }, async () => {
  const f = fixture(), repo = f.repo("overlap"), now = Date.now; let entered, resume, live;
  const deleting = new Promise((r) => { entered = r; }), resumed = new Promise((r) => { resume = r; });
  try {
    assert.equal((await repo.fetch(f.request("overlap", "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201); await repo.queue;
    const store = repo.store, orphan = store.stage(0), protectedStage = store.stage(0);
    const a = new ChunkWriter(store, orphan); await a.write(Uint8Array.of(1, 2, 3)); await a.finish();
    const b = new ChunkWriter(store, protectedStage, temporary ? "canonical" : null, temporary); await b.write(Uint8Array.of(4, 5, 6)); await b.finish();
    const orphanKey = store.exec("SELECT key FROM stage_chunks WHERE stage=?", orphan)[0].key;
    const protectedKey = store.exec(`SELECT key FROM ${temporary ? "temp_chunks" : "stage_chunks"} WHERE stage=?`, protectedStage)[0].key;
    live = { ...owner(), stages: new Set([protectedStage]), temporaries: new Set([protectedStage]), finished: Promise.resolve() };
    repo.jobs.set(live.id, live); store.exec("UPDATE stages SET phase='failed' WHERE id=?", orphan); store.recoveryDue = 0;
    const remove = f.env.SHARED_DATA.delete.bind(f.env.SHARED_DATA); let failProtected = true;
    f.env.SHARED_DATA.delete = async (keys) => {
      if (keys.includes(orphanKey)) { entered(); await resumed; }
      if (keys.includes(protectedKey) && failProtected) throw new Error("protected cleanup failed during recovery");
      return remove(keys);
    };
    const recovering = repo.alarm(); await deleting;
    const view = jobStore(store, live);
    if (temporary) await view.clearTemporary(protectedStage, { defer: true }); else await view.abandon(protectedStage, { defer: true });
    const scheduled = store.recoveryDue; assert.ok(Number.isFinite(scheduled)); resume(); await recovering;
    assert.equal(store.recoveryDue, scheduled, "recovery must not erase a concurrently scheduled retry");
    assert.ok(f.objects.has(protectedKey), "a live job's storage remains protected");
    repo.jobs.delete(live.id); failProtected = false; Date.now = () => scheduled + 1; await repo.alarm(); Date.now = now;
    assert.equal(store.exec("SELECT 1 FROM stages WHERE id=?", protectedStage).length, 0);
    assert.equal(f.objects.size, 0); assert.equal(store.storageAccounting().knownBytes, 0);
    assert.equal(store.exec("SELECT bytes FROM read_scratch_usage WHERE id=1")[0].bytes, 0);
  } finally { resume(); Date.now = now; if (live) repo.jobs.delete(live.id); await repo.queue; f.close(); }
});

for (const first of ["request", "alarm"]) test(`cold ${first} recovery is shared with overlapping requests and alarms`, { timeout: 5000 }, async () => {
  const f = fixture(), ns = "cold-recovery", initial = f.repo(ns), pending = [];
  let entered, resume, cold;
  const deleting = new Promise((resolve) => { entered = resolve; }), resumed = new Promise((resolve) => { resume = resolve; });
  try {
    assert.equal((await initial.fetch(f.request(ns, "api/v1/repository", "admin", { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201); await initial.queue;
    const orphan = initial.store.stage(0), writer = new ChunkWriter(initial.store, orphan);
    await writer.write(Uint8Array.of(1, 2, 3)); await writer.finish();
    initial.store.exec("UPDATE stages SET phase='failed' WHERE id=?", orphan);
    const remove = f.env.SHARED_DATA.delete.bind(f.env.SHARED_DATA); let deletions = 0;
    f.env.SHARED_DATA.delete = async (keys) => { if (++deletions === 1) { entered(); await resumed; } return remove(keys); };
    cold = new f.host.Repository(initial.ctx, f.env); f.instances.set(ns, cold);
    const read = () => cold.fetch(f.request(ns, "api/v1/refs"));
    pending.push(first === "request" ? read() : cold.alarm()); await deleting;
    pending.push(first === "request" ? cold.alarm() : read());
    let served = false; pending.push(read().then((response) => { served = true; return response; }));
    await delay(25);
    assert.equal(cold.store, null, "cold recovery must not expose a partially initialized store");
    assert.equal(served, false, "new jobs must wait for the same cold recovery");
    assert.equal(deletions, 1, "requests and alarms must share one cleanup pass");
    resume(); const results = await Promise.all(pending);
    for (const response of results) if (response instanceof Response) { assert.equal(response.status, 200); await response.arrayBuffer(); }
    assert.equal(f.objects.size, 0); assert.equal(cold.store.exec("SELECT 1 FROM stages WHERE id=?", orphan).length, 0);
    const live = { ...owner(), stages: new Set(), temporaries: new Set(), finished: Promise.resolve() };
    cold.jobs.set(live.id, live);
    const stage = cold.store.stage(0); live.stages.add(stage);
    const active = new ChunkWriter(jobStore(cold.store, live), stage); await active.write(Uint8Array.of(4, 5, 6)); await active.finish();
    cold.store.recoveryDue = 0; await cold.alarm();
    assert.equal(cold.store.exec("SELECT 1 FROM stages WHERE id=?", stage).length, 1, "post-initialization live stages remain protected");
    assert.equal(f.objects.size, 1); cold.jobs.delete(live.id); await cold.store.abandon(stage);
  } finally { resume(); await Promise.allSettled(pending); await cold?.queue; f.close(); }
});
