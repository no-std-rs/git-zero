import assert from "node:assert/strict";
import { test } from "node:test";
import { DatabaseSync } from "node:sqlite";
import { Store } from "../src/storage.mjs";
import { sqliteContext } from "./sqlite.mjs";

test("inventory observation is separated from workload SQL", () => {
  const database = new DatabaseSync(":memory:");
  try {
    const context = sqliteContext(database), execute = context.storage.sql.exec;
    // Known synthetic binding counts exercise subtraction independently of
    // node:sqlite, which does not expose the actual workerd billing counters.
    context.storage.sql.exec = (...args) => Object.assign(execute(...args), { rowsRead: 1, rowsWritten: 0 });
    const store = new Store(context, { BLOBS: {} }, "accounting");
    store.initialize(0);
    const first = store.statistics(), second = store.statistics();
    assert.ok(second.observerUsage.sqlCalls > first.observerUsage.sqlCalls);
    for (const field of ["sqlCalls", "sqlFailures", "sqlRowsRead", "sqlRowsWritten"]) {
      assert.equal(second.usage[field] - first.usage[field], second.observerUsage[field] - first.observerUsage[field]);
    }
    assert.deepEqual(second.usage.r2Gets, 0);
  } finally { database.close(); }
});

test("failed SQL attempts remain counted and invalidate unknown row work", () => {
  const store = Object.assign(Object.create(Store.prototype), {
    ctx: { storage: { sql: { exec() { throw new Error("binding failed after execution"); } } } },
    usage: { sqlCalls: 0, sqlFailures: 0, sqlRowsRead: 0, sqlRowsWritten: 0 }, sqlKnown: true,
    observerUsage: { sqlCalls: 0, sqlFailures: 0, sqlRowsRead: 0, sqlRowsWritten: 0 },
  });
  assert.throws(() => store.exec("SELECT 1"), /binding failed/);
  assert.equal(store.usage.sqlCalls, 1);
  assert.equal(store.usage.sqlFailures, 1);
  assert.equal(store.sqlKnown, false);
  assert.throws(() => store.statistics(), /binding failed/);
  assert.equal(store.usage.sqlCalls, 2);
  assert.equal(store.usage.sqlFailures, 2);
  assert.equal(store.observerUsage.sqlCalls, 1);
  assert.equal(store.observerUsage.sqlFailures, 1);
});
