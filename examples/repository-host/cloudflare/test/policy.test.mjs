import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { gzipSync } from "node:zlib";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, pack, object, entry, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { ChunkWriter, Store } from "../src/storage.mjs";
import { policyRoute } from "../src/policy.mjs";

for (const format of [0, 1]) test(`policy bounds decoded pushes and stored bytes without weakening ref checks format=${format}`, async () => {
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), data = new Map(); let failDelete = false;
  const env = { AUTH_MODE: "gateway", GRANT_KEY, BLOBS: { async get(key) { const value = data.get(key); return value ? { size: value.length, async arrayBuffer() { return value.slice().buffer; } } : null; }, async put(key, value) { data.set(key, new Uint8Array(value).slice()); }, async delete(keys) { if (failDelete) throw new Error("delete failed"); for (const key of keys) data.delete(key); } } };
  let host = new OwnRepository(ctx, env);
  const call = (route, method = "GET", body, level = "admin", headers = {}) => host.fetch(new Request(`http://host/policy.git/${route}`, { method, headers: { "x-git-zero-namespace": "policy", "x-git-zero-grant": mint("policy", level), "content-type": route.startsWith("git-") ? "application/x-git-receive-pack-request" : "application/json", ...headers }, ...(body === undefined ? {} : { body }) }));
  const patch = async (value) => { const response = await call("api/v1/policy", "PATCH", JSON.stringify(value)); assert.equal(response.status, 200, await response.clone().text()); return response.json(); };
  const push = async (updates, packed, gzip = false) => { const body = receive(format, updates, packed), response = await call("git-receive-pack", "POST", gzip ? gzipSync(body) : body, "write", gzip ? { "content-encoding": "gzip" } : {}); assert.equal(response.status, 200, await response.clone().text()); return receiveReport(await response.arrayBuffer()); };
  const fixture = project(format), tip = fixture.root.id.toString("hex"), zero = "0".repeat(tip.length), first = [{ name: "refs/heads/main", old: zero, new: tip }];
  try {
    assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }))).status, 201);
    assert.equal((await call("api/v1/policy", "GET", undefined, "read")).status, 403);
    assert.equal((await call("api/v1/policy?service=git-upload-pack", "PATCH", "{}", "write")).status, 403);
    for (const value of [{ maxPushBytes: -1 }, { maxStoredBytes: "1" }, { allowNonFastForward: 1 }, { unknown: 2 }]) assert.equal((await call("api/v1/policy", "PATCH", JSON.stringify(value))).status, 400);
    await patch({ maxPushBytes: fixture.pack.length - 1 });
    assert.match(await push(first, fixture.pack, true), /single-push size limit exceeded/);
    assert.equal(host.store.exec("SELECT generation FROM repository")[0].generation, 0); assert.equal(host.store.storageAccounting().knownBytes, 0);
    await patch({ maxPushBytes: fixture.pack.length, maxStoredBytes: fixture.pack.length - 1 });
    assert.match(await push(first, fixture.pack), /repository stored-size limit exceeded/);
    assert.equal(host.store.storageAccounting().knownBytes, 0); assert.equal(data.size, 0);
    await patch({ maxStoredBytes: fixture.pack.length }); assert.match(await push(first, fixture.pack, true), /ok refs\/heads\/main/);
    assert.equal(host.store.storageAccounting().knownBytes, fixture.pack.length);
    assert.match(await push([{ name: "refs/tags/same", old: zero, new: tip }], fixture.pack), /ok refs\/tags\/same/);
    assert.equal(host.store.storageAccounting().knownBytes, fixture.pack.length, "shared keys must count once");
    const tree = fixture.objects.find((o) => o.kind === 2).id.toString("hex");
    const rewritten = object(format, 1, `tree ${tree}\nauthor A <a@b> 1 +0000\ncommitter A <a@b> 1 +0000\n\nnew root\n`), other = rewritten.id.toString("hex"), newPack = pack(format, [entry(1, rewritten.body)]);
    await patch({ maxStoredBytes: null, maxPushBytes: null });
    const update = [{ name: "refs/heads/main", old: tip, new: other }];
    assert.match(await push(update, newPack), /ng refs\/heads\/main non-fast-forward/);
    await patch({ allowNonFastForward: true }); assert.match(await push(update, newPack), /ok refs\/heads\/main/);
    assert.match(await push(update, newPack), /ng refs\/heads\/main stale ref/);
    assert.equal(host.store.exec("SELECT generation FROM repository")[0].generation, 3);
    await patch({ maxStoredBytes: 0 });
    assert.match(await push([{ name: "refs/heads/main", old: other, new: zero }, { name: "refs/tags/same", old: tip, new: zero }], pack(format, [])), /ok refs\/heads\/main/);
    assert.ok(host.store.storageAccounting().knownBytes > 0, "unreachable retained bytes count toward storage");
    await host.queue; host = new OwnRepository(ctx, env);
    const persisted = await (await call("api/v1/policy")).json(); assert.equal(persisted.policy.allowNonFastForward, true); assert.equal(persisted.policy.maxStoredBytes, 0);
    env.MAX_PUSH_BYTES = "7"; const constrained = new OwnRepository(ctx, env); host = constrained;
    assert.equal((await patch({ maxPushBytes: 100 })).policy.maxPushBytes, 7);
    await host.queue;
  } finally { db.close(); }
});

test("stored-size migration is bounded, resumable, deduplicated and conservative on cleanup failure", async () => {
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), blobs = new Map(); let fail = false;
  const env = { BLOBS: { async put(key, value) { blobs.set(key, value.slice()); }, async delete(keys) { if (fail) throw new Error("delete unavailable"); for (const key of keys) blobs.delete(key); } } };
  try {
    let store = new Store(ctx, env, "old"); store.initialize(0);
    // Simulate a pre-policy catalog with 150 ownership rows but 75 physical keys.
    for (let i = 0; i < 150; i++) { store.exec("INSERT INTO stage_chunks(stage,seq,key,start,length,sha) VALUES('legacy',?,?,?,?,?)", i, `old-${i % 75}`, i * 3, 3, "a"); blobs.set(`old-${i % 75}`, Uint8Array.of(1, 2, 3)); }
    store.exec("DROP TABLE host_chunk_sizes"); store.exec("DROP TABLE host_storage_usage");
    store = new Store(ctx, env, "old"); assert.equal(store.storageAccounting().ready, false);
    env.MAX_STORED_BYTES = String(75 * 3 + 4);
    assert.throws(() => store.claimChunk("new", 1), /accounting pending/);
    assert.equal((await policyRoute(store, new Request("http://host/old.git/api/v1/policy", { method: "POST" }))).status, 202);
    assert.equal(store.storageAccounting().knownBytes, 64 * 3); assert.equal(store.storageAccounting().ready, false);
    store = new Store(ctx, env, "old");
    for (let i = 0; i < 5 && !store.storageAccounting().ready; i++) await policyRoute(store, new Request("http://host/old.git/api/v1/policy", { method: "POST" }));
    assert.deepEqual(store.storageAccounting(), { ready: true, knownBytes: 75 * 3 });
    assert.equal(store.exec("SELECT COUNT(*) AS n FROM stage_chunks")[0].n, 150, "old ownership tables are not rewritten");
    store.exec("INSERT INTO host_policy(id,max_stored) VALUES(1,?)", 75 * 3 + 4);
    const stage = store.stage(0), writer = new ChunkWriter(store, stage, "temporary"); await writer.write(Uint8Array.of(1, 2, 3, 4)); await writer.finish();
    assert.equal(store.storageAccounting().knownBytes, 75 * 3 + 4);
    assert.equal([...blobs.values()].reduce((sum, value) => sum + value.length, 0), 75 * 3 + 4);
    const extra = new ChunkWriter(store, stage, "extra"); await extra.write(Uint8Array.of(5)); await assert.rejects(extra.finish(), /stored-size limit/);
    fail = true; await store.clearTemporary(stage, { defer: true }); assert.equal(store.storageAccounting().knownBytes, 75 * 3 + 4);
    fail = false; await store.clearTemporary(stage); assert.equal(store.storageAccounting().knownBytes, 75 * 3);
  } finally { db.close(); }
});
