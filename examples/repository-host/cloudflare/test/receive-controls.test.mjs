import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { gzipSync } from "node:zlib";
import { collectControls } from "../src/pack-cache.mjs";
import { receiveControls } from "../src/receive-controls.mjs";
import { CODEC_LIMITS } from "../src/codec.mjs";
import { Store } from "../src/storage.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { project } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
for (const format of [0, 1]) test(`receive controls replay exact bytes across fragmentation and gzip format=${format}`, async () => {
  for (const fragment of [1, 3, 65536]) for (const gzip of [false, true]) {
    const db = new DatabaseSync(":memory:"), owner = { id: crypto.randomUUID(), finalizers: new Set() };
    try {
      const store = new Store(sqliteContext(db), { CODEC_LIMITS, BLOBS: {} }, "repo"), data = project(format);
      const wire = receive(format, [{ name: "refs/heads/main", old: "0".repeat(format ? 64 : 40), new: data.root.id.toString("hex") }], data.pack), input = gzip ? gzipSync(wire) : wire; let offset = 0;
      const body = new ReadableStream({ pull(c) { if (offset === input.length) { c.close(); return; } c.enqueue(input.subarray(offset, offset + fragment)); offset = Math.min(input.length, offset + fragment); } });
      const request = new Request("http://host/repo.git/git-receive-pack", { method: "POST", body, duplex: "half", headers: { "content-type": "application/x-git-receive-pack-request", ...(gzip ? { "content-encoding": "gzip" } : {}) } });
      const replay = await receiveControls(store, module, format, request, owner, 255);
      assert.deepEqual(Buffer.from(await replay.arrayBuffer()), wire, `fragment=${fragment} gzip=${gzip}`);
      assert.equal(store.exec("SELECT COUNT(*) n FROM job_controls")[0].n, 0);
    } finally { for (const fn of owner.finalizers) await fn(); db.close(); }
  }
});


test("bounded upload controls batch SQL independently of transport fragmentation", async () => {
  for (const fragment of [3, 65536]) {
    const db = new DatabaseSync(":memory:"), store = new Store(sqliteContext(db), { CODEC_LIMITS, BLOBS: {} }, "repo");
    const owner = { id: crypto.randomUUID() }, maximum = fragment === 3 ? 4096 : 4 * 1024 * 1024;
    const input = Buffer.alloc(maximum, 97); let offset = 0;
    const body = new ReadableStream({ pull(c) { if (offset === input.length) { c.close(); return; } c.enqueue(input.subarray(offset, offset + fragment)); offset = Math.min(input.length, offset + fragment); } });
    try {
      const before = store.usage.sqlCalls;
      await collectControls(store, owner, new Request("http://host/repo.git/git-upload-pack", { method: "POST", body, duplex: "half" }), maximum);
      assert.ok(store.usage.sqlCalls - before <= 4, "no per-transport-chunk SQL writes");
      const rows = store.exec("SELECT data FROM job_controls WHERE job=? ORDER BY seq", owner.id);
      assert.deepEqual(Buffer.concat(rows.map((row) => Buffer.from(row.data))), input);
    } finally { db.close(); }
  }
});
