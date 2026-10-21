import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { entry, object, pack, wire } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

const SIZE = 9 * 1024 * 1024;
function variable(value) {
  const bytes = [];
  do { const byte = value & 127; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value);
  return Buffer.from(bytes);
}

testRuntimes("{runtime} bounds reconstruction across a complete request", { timeout: 120000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-reconstruction-budget-"));
  let server;
  try {
    server = await serve(directory);
    for (const format of [0, 1]) {
      const base = object(format, 3, Buffer.alloc(SIZE, 122));
      const identity = Buffer.concat([variable(SIZE), variable(SIZE), Buffer.from([0xf0, SIZE & 255, (SIZE >>> 8) & 255, (SIZE >>> 16) & 255])]);
      const packed = pack(format, [entry(3, base.body), entry(7, identity, base.id), entry(7, identity, base.id)]);
      let retry;
      for (const [kind, limit] of [["canonical", SIZE * 2], ["temporary", SIZE], ["work", 65536]]) {
        const namespace = `budget-${kind}-${format}-${randomUUID()}`;
        retry ??= namespace;
        const response = await fetch(`${server.origin}/_test/${namespace}/intake`, {
          method: "POST", headers: { ...headers(format), [`x-test-${kind}-bytes`]: String(limit) }, body: wire(format, packed, base.id),
        });
        const text = await response.text();
        assert.equal(response.status, 400, `${kind} limit must apply across objects: ${text}`);
        assert.equal(JSON.parse(text).codecError, -14, text);
        const stateResponse = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() });
        assert.equal(stateResponse.status, 200);
        const state = await stateResponse.json();
        assert.equal(state.refs, 0); assert.equal(state.objects, 0); assert.equal(state.stagedChunks, 0);
        assert.equal(state.lastIntakeBudget.exhausted, true);
        assert.deepEqual(state.stages, []);
      }
      // A fresh request has a fresh budget, and exact-size reservations remain
      // valid. Identical canonical IDs do not erase physical reconstruction work.
      const namespace = retry;
      const accepted = await fetch(`${server.origin}/_test/${namespace}/intake`, {
        method: "POST", headers: { ...headers(format), "x-test-canonical-bytes": String(SIZE * 4), "x-test-temporary-bytes": String(SIZE * 3), "x-test-work-bytes": String(128 * 1024 * 1024) }, body: wire(format, packed, base.id),
      });
      const result = await accepted.json();
      assert.equal(accepted.status, 200, JSON.stringify(result));
      assert.equal(result.objects, 3); assert.equal(result.refsUpdated, 0);
      assert.equal(result.budgets.canonicalReservedBytes, SIZE * 4);
      assert.equal(result.budgets.temporaryReservedBytes, SIZE * 3);
      assert.ok(result.budgets.workBytes <= 128 * 1024 * 1024);
      assert.equal(result.budgets.exhausted, false);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
