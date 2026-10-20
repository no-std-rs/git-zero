import assert from "node:assert/strict";
import { test } from "node:test";
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
