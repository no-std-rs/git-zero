import test from "node:test";
import assert from "node:assert/strict";
import { ObjectSource } from "../src/source.mjs";
import { ByteBudget, MEMORY_LIMITS, GATEWAY_MEMORY, jobMemoryBytes } from "../src/resources.mjs";
import { CODEC_LIMITS } from "../src/codec.mjs";
import { READ_LIMITS } from "../src/browse.mjs";
test("legacy 8 MiB misses reuse the empty chunk-cache allowance within token and gateway byte pools", async () => {
  for (const memory of [MEMORY_LIMITS, GATEWAY_MEMORY]) {
    const pool = new ByteBudget(96 * 1024 * 1024), reserved = jobMemoryBytes(memory, CODEC_LIMITS, READ_LIMITS, 4 * 1024 * 1024);
    const release = await pool.acquire("repo", reserved, () => {}); let extras = 0;
    const chunk = { key: "legacy", start: 0, length: 8 * 1024 * 1024 };
    const store = { env: { MEMORY: memory, JOB_OWNER: { async extra(n) { extras += n; return pool.acquire("repo", n, () => {}); } } }, exec() { return [chunk]; }, async get(row, cache) { const value = new Uint8Array(row.length); cache.put(row.key, value); return value; } };
    const source = new ObjectSource(store, {}, "job"); source.chunks.put("old", new Uint8Array(1024));
    const bytes = await source.chunkAt({ stage: "published" }, 0);
    assert.equal(bytes.length, 65536); assert.equal(source.chunks.get("old"), undefined);
    assert.equal(extras, memory === MEMORY_LIMITS ? 0 : 13 * 1024 * 1024);
    assert.ok(pool.peak <= pool.limit); assert.equal(pool.used, reserved); release(); source.releaseCaches(); assert.equal(pool.used, 0);
  }
});
