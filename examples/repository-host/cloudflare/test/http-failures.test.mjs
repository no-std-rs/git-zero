import assert from "node:assert/strict";
import { test } from "node:test";
import { DatabaseSync } from "node:sqlite";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { Store } from "../src/storage.mjs";
import { headers } from "./wrangler.mjs";
import { packet, project, receiveReport } from "./fixture.mjs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { startStandalone } from "./standalone.mjs";

const env = { HOST_TOKEN: "git-zero-local-storage-test", BLOBS: { async delete() {} } };
const request = (path = "/_test/errors/state") => new Request("http://fixture" + path, { headers: { ...headers(), "x-git-zero-namespace": "errors" } });

test("SQLite initialization failures return a server error and can recover", async () => {
  const database = new DatabaseSync(":memory:"), ctx = sqliteContext(database), execute = ctx.storage.sql.exec;
  let fail = true;
  ctx.storage.sql.exec = (...args) => { if (fail) throw new Error("injected SQLite unavailable"); return execute(...args); };
  const repository = new OwnRepository(ctx, { ...env, TEST_MODE: "true" });
  try {
    let response = await repository.fetch(request()); assert.equal(response.status, 500); await response.arrayBuffer();
    fail = false;
    response = await repository.fetch(request()); assert.equal(response.status, 200); await response.arrayBuffer();
  } finally { database.close(); }
});

test("failed cold R2 recovery is retried before serving the same repository instance", async () => {
  const database = new DatabaseSync(":memory:"), ctx = sqliteContext(database);
  let deletes = 0, fail = true;
  const settings = { ...env, TEST_MODE: "true", BLOBS: { async delete() { deletes++; if (fail) throw new Error("injected R2 unavailable"); } } };
  const store = new Store(ctx, settings, "errors"); store.initialize(0);
  const stage = store.stage(0); store.exec("INSERT INTO stage_chunks VALUES(?,0,'owned-chunk',0,1,'digest')", stage);
  const repository = new OwnRepository(ctx, settings);
  try {
    let response = await repository.fetch(request()); assert.equal(response.status, 500); await response.arrayBuffer(); assert.equal(deletes, 1);
    fail = false;
    response = await repository.fetch(request()); assert.equal(response.status, 200); const state = await response.json();
    assert.equal(deletes, 2); assert.equal(state.stagedChunks, 0); assert.deepEqual(state.stages, []);
  } finally { database.close(); }
});

test("publication SQL failures remain private and a retry can publish normally", async () => {
  for (const format of [0, 1]) {
    const database = new DatabaseSync(":memory:"), ctx = sqliteContext(database), execute = ctx.storage.sql.exec, objects = new Map();
    let fail = true;
    ctx.storage.sql.exec = (statement, ...bindings) => {
      if (fail && statement.startsWith("UPDATE stages SET phase='published'")) throw new Error("injected publication storage failure");
      return execute(statement, ...bindings);
    };
    const BLOBS = {
      async put(key, data) { objects.set(key, new Uint8Array(data).slice()); },
      async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; },
      async delete(keys) { for (const key of Array.isArray(keys) ? keys : [keys]) objects.delete(key); },
    };
    const repository = new OwnRepository(ctx, { ...env, TEST_MODE: "true", REPO_FORMAT: String(format), BLOBS }), fixture = project(format);
    const capabilities = `report-status-v2 side-band-64k atomic object-format=${format ? "sha256" : "sha1"}`;
    const wire = Buffer.concat([packet(`${"0".repeat(fixture.root.id.length * 2)} ${fixture.root.id.toString("hex")} refs/heads/main\0${capabilities}\n`), Buffer.from("0000"), fixture.pack]);
    const push = () => new Request("http://fixture/errors.git/git-receive-pack", { method: "POST", headers: { ...headers(), "x-git-zero-namespace": "errors" }, body: wire });
    try {
      let response = await repository.fetch(push()); assert.equal(response.status, 500); await response.arrayBuffer();
      response = await repository.fetch(request()); let state = await response.json();
      assert.equal(state.refs, 0); assert.equal(state.objects, 0); assert.equal(state.stagedChunks, 0);
      fail = false;
      response = await repository.fetch(push()); assert.equal(response.status, 200, await response.clone().text()); assert.match(receiveReport(await response.arrayBuffer()), /ok refs\/heads\/main/);
      response = await repository.fetch(request()); state = await response.json(); assert.equal(state.refs, 1); assert.equal(state.objects, 3);
    } finally { database.close(); }
  }
});

test("malformed controls, unavailable wants and invalid content coding remain client errors", async () => {
  const database = new DatabaseSync(":memory:"), repository = new OwnRepository(sqliteContext(database), { ...env, TEST_MODE: "true" });
  const want = Buffer.concat([packet(`want ${"1".repeat(40)}\n`), Buffer.from("0000"), packet("done\n")]);
  try {
    for (const [body, coding] of [[Buffer.from("zzzz"), null], [want, null], [want, "br"], [Buffer.from([1, 2, 3]), "gzip"]]) {
      const response = await repository.fetch(new Request("http://fixture/errors.git/git-upload-pack", {
        method: "POST", headers: { ...headers(), "x-git-zero-namespace": "errors", "content-type": "application/x-git-upload-pack-request", ...(coding ? { "content-encoding": coding } : {}) }, body,
      }));
      assert.equal(response.status, 400, await response.clone().text()); await response.arrayBuffer();
    }
    for (const [format, status] of [[2, 400], [1, 400], [0, 200]]) {
      const response = await repository.fetch(new Request("http://fixture/_test/errors/initialize", { method: "POST", headers: { ...headers(format), "x-git-zero-namespace": "errors" } }));
      assert.equal(response.status, status, await response.clone().text()); await response.arrayBuffer();
    }
  } finally { database.close(); }
});

test("actual workerd returns 500 for cold R2 recovery failures and retries on the same instance", { timeout: 30000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-http-recovery-")); let server;
  try {
    for (const format of [0, 1]) {
      const persist = join(root, String(format)), fixture = project(format);
      server = await startStandalone(persist, { format });
      let response = await fetch(`${server.origin}/_test/recovery/intake`, { method: "POST", headers: headers(format), body: fixture.wire });
      assert.equal(response.status, 200); const received = await response.json();
      response = await fetch(`${server.origin}/_test/recovery/stage/${received.stage}/expire`, { method: "POST", headers: headers() }); assert.equal(response.status, 200); await response.arrayBuffer();
      await server.stop(); server = await startStandalone(persist, { format, deleteFailures: 3 });
      response = await fetch(`${server.origin}/_test/recovery/state`, { headers: headers() }); assert.equal(response.status, 500); await response.arrayBuffer();
      assert.equal(server.storageFaults.deleteFailures, 3);
      response = await fetch(`${server.origin}/_test/recovery/state`, { headers: headers() }); assert.equal(response.status, 200);
      const state = await response.json(); assert.equal(state.refs, 0); assert.equal(state.stagedChunks, 0); assert.deepEqual(state.stages, []);
      await server.stop(); server = null;
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
