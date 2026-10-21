import assert from "node:assert/strict";
import { test } from "node:test";
import { DatabaseSync } from "node:sqlite";
import { Store } from "../src/storage.mjs";
import { publish } from "../src/publication.mjs";

test("publication transaction failures await private-pack reclamation", async () => {
  const failure = new Error("injected transaction failure");
  const events = [];
  const store = {
    ctx: { storage: { transactionSync() { events.push("rollback"); throw failure; } } },
    async abandon(stage) { events.push(stage); await Promise.resolve(); events.push("reclaimed"); },
  };
  await assert.rejects(publish(store, "verified-private-stage"), (error) => error === failure);
  assert.deepEqual(events, ["rollback", "verified-private-stage", "reclaimed"]);
});

function verifiedStage(name) {
  const database = new DatabaseSync(":memory:");
  const storage = {
    sql: { exec(statement, ...bindings) {
      const rows = database.prepare(statement).all(...bindings.map((value) => value instanceof ArrayBuffer ? new Uint8Array(value) : value));
      return { toArray: () => rows };
    } },
    transactionSync(operation) {
      database.exec("BEGIN");
      try { const result = operation(); database.exec("COMMIT"); return result; }
      catch (error) { database.exec("ROLLBACK"); throw error; }
    },
    async setAlarm() {},
  };
  const store = new Store({ storage }, { BLOBS: { async delete() {} } }, "publication");
  const stage = store.stage(0), oid = "1".repeat(40);
  store.exec("UPDATE stages SET phase='verified',expected=1 WHERE id=?", stage);
  store.exec("INSERT INTO stage_objects(stage,node,wire_kind,packed_size,compressed_offset,end_offset,oid,kind,size,depth,verified,scanned) VALUES(?,12,1,1,13,14,?,1,0,0,1,1)", stage, oid);
  store.exec("INSERT INTO stage_commands(stage,seq,name,old_oid,new_oid,branch,creation,deletion) VALUES(?,0,?,?,?,0,1,0)", stage, Buffer.from(name), "0".repeat(40), oid);
  return { database, store, stage, oid };
}

test("deep ref publication batches indexed ancestor lookups", async (t) => {
  const name = `refs/tags/${"x/".repeat(4096)}tip`;
  const { database, store, stage, oid } = verifiedStage(name);
  try {
    const before = store.usage.sqlCalls;
    const outcomes = await publish(store, stage);
    const calls = store.usage.sqlCalls - before;
    assert.equal(outcomes.length, 1); assert.equal(outcomes[0].error, null);
    assert.equal(store.exec("SELECT oid FROM refs WHERE name=?", Buffer.from(name))[0].oid, oid);
    assert.ok(calls < 200, `deep ref used ${calls} SQL calls`);
    t.diagnostic(`4098 raw ref separators: ${calls} publication SQL calls`);
  } finally { database.close(); }
});

test("ancestor batches preserve raw bytes and a final partial batch", async () => {
  const prefix = Buffer.concat([Buffer.from(`refs/tags/${"x/".repeat(70)}`), Buffer.from([255])]);
  const child = Buffer.concat([prefix, Buffer.from("/child")]);
  const { database, store, stage, oid } = verifiedStage(child);
  try {
    for (const name of [prefix, Buffer.concat([prefix, Buffer.from(".before")])]) store.exec("INSERT INTO refs(name,oid) VALUES(?,?)", name, oid);
    const outcomes = await publish(store, stage);
    assert.equal(outcomes[0].error, "ref name conflicts");
    assert.equal(store.exec("SELECT COUNT(*) AS count FROM refs WHERE name=?", child)[0].count, 0);
  } finally { database.close(); }
});

test("published stages discard request controls while retaining objects and ref outcomes", async () => {
  const { database, store, stage, oid } = verifiedStage("refs/tags/published");
  try {
    store.exec("INSERT INTO stage_options(stage,seq,value) VALUES(?,0,?)", stage, Buffer.alloc(60000, 120));
    store.exec("INSERT INTO stage_shallow(stage,oid) VALUES(?,?)", stage, oid);
    const outcomes = await publish(store, stage);
    assert.equal(outcomes[0].error, null);
    assert.equal(Buffer.from(outcomes[0].name).toString(), "refs/tags/published");
    assert.equal(store.exec("SELECT phase FROM stages WHERE id=?", stage)[0].phase, "published");
    for (const table of ["stage_commands", "stage_options", "stage_shallow"]) {
      assert.equal(store.exec(`SELECT COUNT(*) AS count FROM ${table} WHERE stage=?`, stage)[0].count, 0, `${table} must not retain published request controls`);
    }
    assert.equal(store.exec("SELECT oid FROM stage_objects WHERE stage=?", stage)[0].oid, oid);
    assert.equal(store.exec("SELECT oid FROM refs")[0].oid, oid);
  } finally { database.close(); }
});

test("repeated nested-tag targets share one publication peel", async (t) => {
  const { database, store, stage, oid } = verifiedStage("refs/tags/repeated-0");
  const commands = 256, depth = 32;
  try {
    let target = oid;
    for (let index = 0; index < depth; index++) {
      const id = (index + 2).toString(16).padStart(40, "0"), node = 1000 + index;
      store.exec("INSERT INTO stage_objects(stage,node,wire_kind,packed_size,compressed_offset,end_offset,oid,kind,size,depth,verified,scanned) VALUES(?,?,4,1,?,?,?,4,0,0,1,1)", stage, node, node + 1, node + 2, id);
      store.exec("INSERT INTO stage_edges(stage,node,dst,kind,relation) VALUES(?,?,?,?,2)", stage, node, target, index ? 4 : 1);
      target = id;
    }
    store.exec("UPDATE stages SET expected=? WHERE id=?", depth + 1, stage);
    store.exec("UPDATE stage_commands SET new_oid=? WHERE stage=?", target, stage);
    for (let index = 1; index < commands; index++) store.exec("INSERT INTO stage_commands(stage,seq,name,old_oid,new_oid,branch,creation,deletion) VALUES(?,?,?,?,?,0,1,0)", stage, index, Buffer.from(`refs/tags/repeated-${index}`), "0".repeat(40), target);
    const before = store.usage.sqlCalls;
    const outcomes = await publish(store, stage), calls = store.usage.sqlCalls - before;
    assert.equal(outcomes.length, commands); assert.ok(outcomes.every((item) => item.error === null));
    assert.equal(store.exec("SELECT COUNT(*) AS count FROM refs WHERE oid=? AND peeled=?", target, oid)[0].count, commands);
    assert.ok(calls < commands * 8 + 128, `repeated nested tag used ${calls} publication SQL calls`);
    t.diagnostic(`${commands} refs / ${depth} nested tags: ${calls} publication SQL calls`);
  } finally { database.close(); }
});
