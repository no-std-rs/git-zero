import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, pack, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { changes } from "../src/changes.mjs";
import { Store } from "../src/storage.mjs";

for (const format of [0, 1]) test(`push record is atomic, actor-exact, resumable and survives later publications format=${format}`, async () => {
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), objects = new Map();
  const env = { AUTH_MODE: "gateway", GRANT_KEY, BLOBS: { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); } } };
  let host = new OwnRepository(ctx, env);
  const actor = "\uFEFFactor\n\0opaque ", zero = "0".repeat(format ? 64 : 40), fixture = project(format), tip = fixture.root.id.toString("hex");
  const call = (route, method = "GET", body, access = "read") => host.fetch(new Request(`http://host/log.git/${route}`, { method, headers: { "x-git-zero-namespace": "log", "x-git-zero-grant": mint("log", access, { actor }), "content-type": route === "git-receive-pack" ? "application/x-git-receive-pack-request" : "application/json" }, ...(body === undefined ? {} : { body }) }));
  async function get(query = "") { const response = await call(`api/v1/changes${query}`); assert.equal(response.status, 200, await response.clone().text()); return response.json(); }
  async function push(updates, packed = pack(format, [])) { const response = await call("git-receive-pack", "POST", receive(format, updates, packed), "write"); assert.equal(response.status, 200, await response.clone().text()); return receiveReport(await response.arrayBuffer()); }
  try {
    assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }), "admin")).status, 201);
    assert.deepEqual((await get()).publications, []);
    const updates = Array.from({ length: 23 }, (_, n) => ({ name: n ? `refs/tags/tag-${n}` : "refs/heads/main", old: zero, new: tip }));
    assert.ok(!(await push(updates, fixture.pack)).includes("ng "));
    const first = await get("?limit=3"); assert.ok(first.nextCursor); assert.equal(first.completedThrough, 0); assert.equal(first.publications[0].complete, false); assert.equal(first.publications[0].actor, actor);
    const time = first.publications[0].time; assert.equal(host.store.exec("SELECT last_publication FROM host_settings")[0].last_publication, time);
    assert.match(await push([{ name: "refs/tags/later", old: zero, new: tip }]), /ok refs\/tags\/later/);
    await host.queue; host = new OwnRepository(ctx, env);
    const seen = [...first.publications[0].updates]; let cursor = first.nextCursor, completed = 0;
    while (cursor) { const page = await get(`?cursor=${cursor}&limit=4`); assert.equal(page.throughGeneration, 1); assert.ok(page.completedThrough >= completed); completed = page.completedThrough; seen.push(...page.publications.flatMap((event) => event.updates)); for (const event of page.publications) { assert.equal(event.actor, actor); assert.equal(event.time, time); } cursor = page.nextCursor; }
    assert.equal(completed, 1); assert.deepEqual(seen.map(({ name, oldId, newId }) => ({ name, old: oldId, new: newId })), updates);
    const second = await get("?after=1"); assert.equal(second.publications[0].generation, 2); assert.equal(second.completedThrough, 2);
    assert.match(await push([{ name: "refs/tags/later", old: tip, new: zero }]), /ok refs\/tags\/later/);
    const deleted = await get("?after=2"); assert.deepEqual(deleted.publications[0].updates.map(({ oldId, newId }) => [oldId, newId]), [[tip, zero]]);
    assert.match(await push([{ name: "refs/heads/main", old: tip, new: tip }]), /ok refs\/heads\/main/);
    assert.match(await push([{ name: "refs/heads/main", old: zero, new: tip }]), /ng refs\/heads\/main stale ref/);
    assert.equal((await get("?after=3")).publications.length, 0); assert.equal(host.store.exec("SELECT generation FROM repository")[0].generation, 3);
    host.store.exec("CREATE TRIGGER fail_record BEFORE INSERT ON publication_updates BEGIN SELECT RAISE(ABORT,'injected log failure'); END");
    const failed = await call("git-receive-pack", "POST", receive(format, [{ name: "refs/tags/rollback", old: zero, new: tip }], pack(format, [])), "write");
    assert.equal(failed.status, 500); await failed.arrayBuffer();
    assert.equal(host.store.exec("SELECT 1 FROM refs WHERE name=?", Buffer.from("refs/tags/rollback")).length, 0);
    assert.equal(host.store.exec("SELECT generation FROM repository")[0].generation, 3);
    assert.equal(host.store.exec("SELECT COUNT(*) AS n FROM publications")[0].n, 3);
    assert.equal((await call("api/v1/changes?limit=101")).status, 400); assert.equal((await call("api/v1/changes?after=4")).status, 409);
    assert.equal((await call("api/v1/changes?cursor=invalid")).status, 400);
    host.store.exec("DROP TRIGGER fail_record");
    const wide = Array.from({ length: 40 }, (_, i) => ({ name: `refs/tags/${i}-${'"'.repeat(6000)}`, old: zero, new: tip }));
    assert.ok(!(await push(wide)).includes("ng "));
    let page = await get("?after=3&limit=100"), count = 0;
    assert.ok(page.nextCursor, "byte budget must split an event below the count limit");
    for (;;) { assert.ok(Buffer.byteLength(JSON.stringify(page)) <= 512 * 1024); count += page.publications.flatMap((p) => p.updates).length; if (!page.nextCursor) break; page = await get(`?cursor=${page.nextCursor}&limit=100`); }
    assert.equal(count, wide.length); assert.equal(page.completedThrough, 4);
    await host.queue;
  } finally { db.close(); }
});

test("old repositories expose a durable journal coverage boundary without rewriting history", () => {
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), env = { BLOBS: {} };
  try {
    db.exec("CREATE TABLE repository(id INTEGER PRIMARY KEY,format INTEGER NOT NULL,generation INTEGER NOT NULL DEFAULT 0,object_count INTEGER NOT NULL DEFAULT 0,head BLOB NOT NULL)");
    db.prepare("INSERT INTO repository VALUES(1,0,17,0,?)").run(Buffer.from("refs/heads/main"));
    const store = new Store(ctx, env, "legacy");
    assert.equal(changes(store, new Request("http://host/legacy.git/api/v1/changes?after=0")).status, 409);
    assert.equal(changes(store, new Request("http://host/legacy.git/api/v1/changes?after=17")).status, 200);
    store.exec("UPDATE repository SET generation=18"); new Store(ctx, env, "legacy");
    assert.equal(store.exec("SELECT generation FROM publication_start")[0].generation, 17);
  } finally { db.close(); }
});
