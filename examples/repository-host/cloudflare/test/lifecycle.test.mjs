import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { setImmediate as tick } from "node:timers/promises";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, packet, receiveReport } from "./fixture.mjs";
import { PURGE_PAGE } from "../src/lifecycle.mjs";

function pushed(format) {
  const fixture = project(format);
  const caps = `report-status-v2 atomic side-band-64k object-format=${format ? "sha256" : "sha1"}`;
  fixture.wire = Buffer.concat([packet(`${"0".repeat(fixture.root.id.length * 2)} ${fixture.root.id.toString("hex")} refs/heads/main\0${caps}\n`), Buffer.from("0000"), fixture.pack]);
  return fixture;
}

function fixture() {
  const database = new DatabaseSync(":memory:"), ctx = sqliteContext(database), objects = new Map(), batches = [];
  const BLOBS = {
    async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; },
    async put(key, data) { objects.set(key, new Uint8Array(data).slice()); },
    async delete(keys) { batches.push(keys); for (const key of keys) objects.delete(key); },
    async list({ prefix, limit }) { const keys = [...objects.keys()].filter((key) => key.startsWith(prefix)).sort(); return { objects: keys.slice(0, limit).map((key) => ({ key })), truncated: keys.length > limit }; },
  };
  const env = { AUTH_MODE: "gateway", GRANT_KEY, BLOBS };
  let repository = new OwnRepository(ctx, env);
  const request = (method = "GET", path = "api/v1/repository", body, access = "admin") => new Request(`http://host/repo.git/${path}`, { method, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", access), "content-type": path === "git-receive-pack" ? "application/x-git-receive-pack-request" : "application/json" }, ...(body === undefined ? {} : { body: body instanceof Uint8Array ? body : JSON.stringify(body) }) });
  return { database, ctx, objects, batches, BLOBS, request, env, get repository() { return repository; }, restart() { repository = new OwnRepository(ctx, env); }, call: (...args) => repository.fetch(request(...args)) };
}
async function value(response, status = 200) { const data = await response.json(); assert.equal(response.status, status, JSON.stringify(data)); return data; }

for (const format of [0, 1]) test(`lifecycle creation, metadata and real codec publication format=${format}`, async () => {
  const f = fixture();
  try {
    for (const access of ["read", "write", "admin"]) assert.equal((await f.call("GET", "info/refs?service=git-upload-pack", undefined, access)).status, 404);
    for (const access of ["read", "write"]) for (const service of ["", "?service=git-upload-pack", "?service=git-receive-pack"]) {
      assert.equal((await f.call("PUT", `api/v1/repository${service}`, {}, access)).status, 403);
      assert.equal((await f.call("DELETE", `api/v1/repository${service}`, undefined, access)).status, 403);
    }
    const settings = { objectFormat: format ? "sha256" : "sha1", defaultBranch: "main" };
    const first = await value(await f.call("PUT", "api/v1/repository", settings), 201);
    assert.equal(f.repository.store.usage.r2FenceGets, 1);
    assert.equal(f.repository.store.usage.r2Gets, 0, "a namespace fence lookup is separate from pack reads");
    assert.equal(first.objectFormat, settings.objectFormat); assert.equal(first.generation, 0); assert.equal(first.head.oid, null);
    assert.equal(first.objectCount, 0); assert.equal(first.storedPackBytes, 0); assert.equal(first.lastPublication, null);
    assert.deepEqual(await value(await f.call("PUT", "api/v1/repository", settings)), first);
    assert.equal((await f.call("PUT", "api/v1/repository", { ...settings, defaultBranch: "different" })).status, 409);
    const publication = pushed(format), response = await f.call("POST", "git-receive-pack", publication.wire, "write");
    assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs\/heads\/main/);
    const described = await value(await f.call());
    assert.equal(described.head.oid, publication.root.id.toString("hex")); assert.equal(described.generation, 1);
    assert.equal(described.objectCount, 3); assert.equal(described.storedPackBytes, publication.pack.length); assert.ok(described.lastPublication > 0);
    f.restart(); assert.deepEqual(await value(await f.call()), described);
  } finally { f.database.close(); }
});

test("custom default branch is validated, pinned and kept unborn until published", async () => {
  const f = fixture();
  try {
    for (const defaultBranch of ["", "bad..name", "x.lock/y", "a//b", "x/", "a b", "a@{b", "\\bad", "x".repeat(241), "\ud800", "HEAD"]) assert.equal((await f.call("PUT", "api/v1/repository", { defaultBranch })).status, 400, defaultBranch);
    assert.equal((await f.call("PUT", "api/v1/repository", { objectFormat: "invalid" })).status, 400);
    const description = await value(await f.call("PUT", "api/v1/repository", { defaultBranch: "analysis/日本語" }), 201);
    assert.equal(description.head.symbolic, "refs/heads/analysis/日本語");
    const response = await f.call("POST", "git-receive-pack", pushed(0).wire, "write"); assert.equal(response.status, 200, await response.clone().text()); assert.match(receiveReport(await response.arrayBuffer()), /ok refs/);
    assert.deepEqual((await value(await f.call())).head, { symbolic: description.head.symbolic, oid: null });
  } finally { f.database.close(); }
});

test("delete bounds every batch, clears SQLite, and fences namespace reuse across restarts", async () => {
  const f = fixture();
  try {
    await value(await f.call("PUT", "api/v1/repository", {}), 201);
    for (let n = 0; n < PURGE_PAGE + 7; n++) f.objects.set(`repositories/repo/orphan/${n}`, new Uint8Array([1]));
    f.objects.set("repositories/repo-other/chunks/protected", new Uint8Array([2]));
    await value(await f.call("DELETE"), 202);
    assert.equal(f.batches[0].length, PURGE_PAGE);
    assert.equal((await f.call("GET", "info/refs?service=git-upload-pack", undefined, "read")).status, 404);
    assert.equal((await f.call("PUT", "api/v1/repository", {})).status, 410);
    f.restart(); await value(await f.call("DELETE"), 202);
    const deleted = await value(await f.call("DELETE")); assert.equal(deleted.complete, true);
    assert.equal(f.database.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").get().n, 0);
    assert.equal([...f.objects.keys()].filter((key) => key.startsWith("repositories/repo/")).length, 0);
    assert.ok(f.objects.has("repositories/repo-other/chunks/protected"));
    assert.ok(f.objects.has("repository-tombstones/v1/repo"));
    assert.ok(f.batches.every((batch) => batch.length <= PURGE_PAGE));
    f.restart(); assert.equal((await f.call("PUT", "api/v1/repository", {})).status, 410);
    assert.equal((await value(await f.call("DELETE"))).complete, true);
  } finally { f.database.close(); }
});

test("external fence survives a crash between native SQLite clearing and tombstone save", async () => {
  const f = fixture();
  try {
    await value(await f.call("PUT", "api/v1/repository", {}), 201);
    const put = f.ctx.storage.put; let fail = true;
    f.ctx.storage.put = async (key, state) => { if (fail && state.state === "deleted") throw new Error("crash after deleteAll"); return put(key, state); };
    assert.equal((await f.call("DELETE")).status, 500);
    assert.equal(f.database.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table'").get().n, 0);
    fail = false; f.restart();
    assert.equal((await f.call("PUT", "api/v1/repository", {})).status, 410);
    assert.equal((await f.call("GET", "info/refs?service=git-upload-pack", undefined, "read")).status, 404);
    await value(await f.call("DELETE"));
  } finally { f.database.close(); }
});

test("failed R2 deletion retains the retired barrier and retry reclaims remaining bytes", async () => {
  const f = fixture();
  try {
    await value(await f.call("PUT", "api/v1/repository", {}), 201);
    f.objects.set("repositories/repo/chunks/a", new Uint8Array([1]));
    const remove = f.BLOBS.delete; let fail = true;
    f.BLOBS.delete = async (keys) => { if (fail) throw new Error("delete unavailable"); return remove(keys); };
    assert.equal((await f.call("DELETE")).status, 500);
    assert.equal((await f.call("GET", "info/refs?service=git-upload-pack", undefined, "read")).status, 404);
    fail = false; f.restart(); await value(await f.call("DELETE"), 202); await value(await f.call("DELETE"));
    assert.equal(f.objects.has("repositories/repo/chunks/a"), false);
  } finally { f.database.close(); }
});

test("a push queued behind deletion cannot publish into the retired namespace", async () => {
  const f = fixture();
  try {
    await value(await f.call("PUT", "api/v1/repository", {}), 201);
    const stream = await f.call("GET", "info/refs?service=git-upload-pack", undefined, "read");
    const previous = f.repository.queue, deleting = f.call("DELETE");
    while (f.repository.queue === previous) await tick();
    const pushing = f.call("POST", "git-receive-pack", pushed(0).wire, "write");
    await stream.arrayBuffer(); await value(await deleting);
    assert.equal((await pushing).status, 404);
    assert.equal([...f.objects.keys()].some((key) => key.startsWith("repositories/repo/")), false);
  } finally { f.database.close(); }
});

test("a cold object refuses a different namespace before touching retirement state", async () => {
  const f = fixture();
  try {
    await value(await f.call("PUT", "api/v1/repository", {}), 201);
    f.objects.set("repository-tombstones/v1/other", new Uint8Array([1]));
    f.restart();
    const response = await f.repository.fetch(new Request("http://host/other.git/api/v1/repository", { method: "DELETE", headers: { "x-git-zero-namespace": "other", "x-git-zero-grant": mint("other", "admin") } }));
    assert.equal(response.status, 404);
    assert.equal((await value(await f.call())).state, "active");
  } finally { f.database.close(); }
});

test("cold retirement-fence reads use the same bounded transient retries as data reads", async () => {
  const f = fixture(), get = f.BLOBS.get; let attempts = 0;
  try {
    f.BLOBS.get = async (key) => { if (attempts++ < 2) { const error = new Error("network connection lost"); error.retryable = true; throw error; } return get(key); };
    await value(await f.call("PUT", "api/v1/repository", {}), 201);
    assert.equal(f.repository.store.usage.r2FenceGets, 3);
    assert.equal(f.repository.store.usage.r2Gets, 0);
    assert.equal(f.repository.store.usage.r2Retries, 2);
    assert.equal(f.repository.store.usage.r2Failures, 2);
  } finally { f.database.close(); }
});
