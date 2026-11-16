import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay, setImmediate as tick } from "node:timers/promises";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";

const env = { AUTH_MODE: "gateway", GRANT_KEY, TEST_MODE: "true", BLOBS: { async get() { return null; } } };
function request(path, expiry, init = {}) {
  return new Request(`http://host/${path}`, { ...init, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", "admin", expiry === undefined ? {} : { expiry }), ...init.headers } });
}

test("a grant admitted before expiry survives the preceding response stream", { timeout: 15000 }, async () => {
  const database = new DatabaseSync(":memory:"), repository = new OwnRepository(sqliteContext(database), env);
  repository.gate.limit = 1;
  let first;
  try {
    assert.equal((await repository.fetch(request("repo.git/api/v1/repository", undefined, { method: "PUT", headers: { "content-type": "application/json" }, body: "{}" }))).status, 201);
    first = await repository.fetch(request("repo.git/info/refs?service=git-upload-pack"));
    assert.equal(first.status, 200);
    const held = repository.queue;
    let finished = false; held.then(() => { finished = true; });
    const expiry = Math.floor(Date.now() / 1000) + 3;
    const queued = repository.fetch(request("_test/repo/state", expiry));
    while (repository.queue === held) await tick();
    await delay(Math.max(0, expiry * 1000 - Date.now()) + 30);
    assert.equal(finished, false, "the prior unread Git stream still owns scratch past expiry");
    assert.ok(Math.floor(Date.now() / 1000) >= expiry);
    const invalid = await Promise.race([
      repository.fetch(request("_test/repo/state", expiry)),
      delay(1000).then(() => { throw new Error("invalid arrival waited behind the stream"); }),
    ]);
    assert.equal(invalid.status, 404); assert.equal(invalid.headers.has("www-authenticate"), false);
    await first.arrayBuffer();
    const response = await queued;
    assert.equal(response.status, 200, await response.clone().text());
    assert.equal((await response.json()).refs, 0);
    await repository.queue;
  } finally { await first?.body?.cancel().catch(() => {}); database.close(); }
});

test("authorization precedes queued alarm recovery as well as response ownership", { timeout: 15000 }, async () => {
  let release, started;
  const recovering = new Promise((resolve) => { started = resolve; });
  const held = new Promise((resolve) => { release = resolve; });
  const database = new DatabaseSync(":memory:"), repository = new OwnRepository(sqliteContext(database), env);
  await (await repository.fetch(request("_test/repo/state"))).json();
  repository.store.recover = async () => { started(); await held; };
  repository.store.recoveryDue = 0;
  // Simulate cold activation recovery: warm maintenance never gates requests.
  const alarm = repository.alarm(); repository.coldRecovery = alarm; await recovering;
  const before = repository.queue, expiry = Math.floor(Date.now() / 1000) + 2;
  const queued = repository.fetch(request("_test/repo/state", expiry));
  while (repository.queue === before) await tick();
  await delay(Math.max(0, expiry * 1000 - Date.now()) + 30);
  release(); await alarm;
  assert.equal((await queued).status, 200);
  await repository.queue; database.close();
});
