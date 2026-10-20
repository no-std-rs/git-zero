import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { project } from "./fixture.mjs";
import { startStandalone } from "./standalone.mjs";
import { headers } from "./wrangler.mjs";
import { ByteCache, Store, sha256 } from "../src/storage.mjs";

const lost = () => new Error("Network connection lost.");
function store(blobs) {
  return Object.assign(Object.create(Store.prototype), { blobs, usage: { r2Puts: 0, r2WrittenBytes: 0, r2Deletes: 0, r2Gets: 0, r2ReadBytes: 0, r2Retries: 0 } });
}

test("idempotent chunk writes and deletes tolerate lost acknowledgments", async () => {
  const objects = new Map(), data = Uint8Array.of(1, 2, 3);
  let puts = 0, deletes = 0;
  const storage = store({
    async put(key, bytes) { objects.set(key, bytes.slice()); if (++puts === 1) throw lost(); },
    async delete(key) { objects.delete(key); if (++deletes === 1) throw lost(); },
  });
  await storage.put("owned-chunk", data);
  assert.deepEqual(objects.get("owned-chunk"), data);
  assert.equal(storage.usage.r2Puts, 2);
  await storage.delete("owned-chunk");
  assert.equal(objects.size, 0);
  assert.equal(storage.usage.r2Deletes, 2);
});

test("chunk reads retry both connection and response-body failures before caching", async () => {
  const data = Uint8Array.of(1, 2, 3), cache = new ByteCache(1024);
  const chunk = { key: "owned-chunk", length: data.length, sha: await sha256(data) };
  let reads = 0;
  const storage = store({ async get() {
    if (++reads === 1) throw lost();
    const bodyFails = reads === 2;
    return { size: data.length, async arrayBuffer() { if (bodyFails) throw lost(); return data.buffer; } };
  } });
  assert.deepEqual(await storage.get(chunk, cache), data);
  assert.deepEqual(await storage.get(chunk, cache), data);
  assert.equal(reads, 3);
  assert.equal(storage.usage.r2Gets, 3);
  assert.equal(storage.usage.r2Retries, 2);
  assert.equal(storage.usage.r2ReadBytes, data.length);
});

test("storage retry budgets terminate and preserve permanent failures", async () => {
  let reads = 0;
  const storage = store({ async get() { reads++; throw lost(); } });
  await assert.rejects(storage.get({ key: "owned-chunk" }, new ByteCache(1024)), /Network connection lost/);
  assert.equal(reads, 3);
  let puts = 0;
  const forbidden = new Error("R2 S3 PUT failed (403)");
  storage.blobs.put = async () => { puts++; throw forbidden; };
  await assert.rejects(storage.put("owned-chunk", Uint8Array.of(1)), (error) => error === forbidden);
  assert.equal(puts, 1);
});

test("storage retries do not mask invalid or corrupted chunk data", async () => {
  const data = Uint8Array.of(1), cache = new ByteCache(1024);
  const chunk = { key: "owned-chunk", length: data.length, sha: await sha256(data) };
  let reads = 0;
  const storage = store({ async get() { reads++; return { size: 1, async arrayBuffer() { return Uint8Array.of(2).buffer; } }; } });
  await assert.rejects(storage.get(chunk, cache), /stored chunk hash differs/);
  assert.equal(reads, 1); assert.equal(cache.get(chunk.key), undefined);
});


test("signed S3 reads recover dropped connections, truncated bodies and 503 responses", { timeout: 30000 }, async () => {
  for (const getFailure of ["connection", "body", "status"]) {
    const directory = await mkdtemp(join(tmpdir(), "git-zero-r2-retry-"));
    let server;
    try {
      server = await startStandalone(directory, { getFailure });
      const fixture = project(0), namespace = "retry";
      const intake = await fetch(`${server.origin}/_test/${namespace}/intake`, { method: "POST", headers: headers(), body: fixture.wire });
      assert.equal(intake.status, 200, await intake.clone().text());
      const received = await intake.json();
      const row = await (await fetch(`${server.origin}/_test/${namespace}/stage/${received.stage}`, { headers: headers() })).json();
      const root = row.objects.find((item) => item.oid === fixture.root.id.toString("hex"));
      const canonical = await fetch(`${server.origin}/_test/${namespace}/object/${received.stage}?node=${root.node}&data=1`, { headers: headers() });
      assert.equal(canonical.status, 200, await canonical.clone().text());
      assert.deepEqual(Buffer.from(await canonical.arrayBuffer()), fixture.root.body);
      assert.equal(server.storageFaults.injected, 1);
      const state = await (await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() })).json();
      assert.equal(state.usage.r2Retries, 1);
    } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
  }
});
