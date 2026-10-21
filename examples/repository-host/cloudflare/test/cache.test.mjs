import assert from "node:assert/strict";
import { test } from "node:test";
import { ByteCache, CacheBuffers, PAGE_BYTES } from "../src/storage.mjs";
import { ObjectSource } from "../src/source.mjs";

test("verified base metadata shares canonical and physical aliases within one request", () => {
  const oid = "1".repeat(40), base = { stage: "published", node: 12, oid, verified: 1, kind: 3, size: 7 };
  let reads = 0;
  const store = {
    object(stage, id) { reads++; assert.equal(stage, "intake"); assert.equal(id, oid); return { ...base }; },
    node() { assert.fail("resolved physical aliases must reuse the verified row"); },
  };
  const source = new ObjectSource(store, {}, "intake"), received = { stage: "intake", node: 42, wire_base_id: oid };
  assert.deepEqual(source.base(received), base);
  assert.deepEqual(source.base({ ...received, base_stage: base.stage, base_node: base.node }), base);
  assert.deepEqual(source.base(received), base);
  assert.equal(reads, 1);
  assert.deepEqual(new ObjectSource(store, {}, "intake").base(received), base);
  assert.equal(reads, 2, "a new request must consult its own visible catalog");
});

test("base metadata keeps stage identity, retries unresolved rows and releases its bounded cache", () => {
  let reads = 0;
  const rows = new Map();
  const store = { node(stage, node) { reads++; return rows.get(`${stage}:${node}`); } };
  const source = new ObjectSource(store, {}, "intake"), lookup = (stage, node) => source.base({ stage, wire_base_offset: node });
  assert.equal(lookup("one", 12), undefined);
  rows.set("one:12", { stage: "one", node: 12, oid: "1".repeat(40), verified: 0 });
  assert.equal(lookup("one", 12).verified, 0);
  rows.set("one:12", { stage: "one", node: 12, oid: "1".repeat(40), verified: 1 });
  assert.equal(lookup("one", 12).verified, 1);
  rows.set("two:12", { stage: "two", node: 12, oid: "2".repeat(40), verified: 1 });
  assert.equal(lookup("two", 12).stage, "two");
  assert.equal(reads, 4);
  for (let node = 13; node < 1037; node++) {
    rows.set(`one:${node}`, { stage: "one", node, oid: node.toString(16).padStart(40, "0"), verified: 1 });
    assert.equal(lookup("one", node).node, node);
  }
  assert.ok(source.bases.size <= 256);
  const before = reads;
  assert.equal(lookup("one", 12).stage, "one");
  assert.equal(reads, before + 1, "evicted metadata must be loaded again");
  source.releaseCaches(); assert.equal(source.bases.size, 0);
});

test("cache entry count bounds empty and tiny object representations independently of byte budget", () => {
  const cache = new ByteCache(1024, 16);
  for (let index = 0; index < 10000; index++) cache.put(`entry-${index}`, new Uint8Array(index % 2));
  assert.equal(cache.entries.size, 16); assert.equal(cache.size, 8);
  assert.equal(cache.get("entry-0"), undefined);
  const last = cache.get("entry-9984"); assert.equal(last.length, 0);
  cache.put("extra", new Uint8Array(1024));
  assert.equal(cache.entries.size, 2); assert.equal(cache.size, 1024);
  cache.put("extra", new Uint8Array(1)); assert.equal(cache.size, 1);
  cache.put("too-big", new Uint8Array(1025)); assert.equal(cache.entries.size, 2);
  assert.equal(cache.peak, 1024);
});

test("owned caches release shared buffers only after their last owner retires", () => {
  const ownership = new CacheBuffers(), canonical = new ByteCache(4, 2, ownership), representation = new ByteCache(4, 2, ownership);
  const shared = new Uint8Array([1, 2, 3, 4]);
  canonical.put("object", shared); representation.put("entry", shared);
  canonical.put("object", shared);
  assert.equal(shared.byteLength, 4, "replacement must acquire its ownership before releasing the old one");
  const next = new Uint8Array([5, 6, 7, 8]); canonical.put("next", next);
  assert.deepEqual(representation.get("entry"), new Uint8Array([1, 2, 3, 4]));
  representation.clear(); assert.equal(shared.byteLength, 0);
  canonical.clear(); assert.equal(next.byteLength, 0);
  assert.equal(canonical.size, 0); assert.equal(representation.size, 0);
});

test("borrowed caches preserve retired views and rejected inputs keep their buffers", () => {
  const borrowed = new ByteCache(4), retained = new Uint8Array([1, 2, 3, 4]);
  borrowed.put("old", retained); borrowed.put("next", new Uint8Array(4)); borrowed.clear();
  assert.equal(retained.byteLength, 4);
  const owned = new ByteCache(4, 2, new CacheBuffers()), oversized = new Uint8Array(8);
  owned.put("large", oversized); assert.equal(oversized.byteLength, 8);
  assert.throws(() => owned.put("partial", oversized.subarray(0, 4)), /complete buffer/);
  assert.equal(owned.entries.size, 0); assert.equal(oversized.byteLength, 8);
});

test("incremental readers pin their current page across owned chunk eviction", async () => {
  const bytes = Uint8Array.from({ length: PAGE_BYTES }, (_, i) => i % 251);
  const store = {
    exec: () => [{ key: "chunk", start: 12, length: bytes.length }],
    async get(chunk, cache) { const hit = cache.get(chunk.key); if (hit) return hit; cache.put(chunk.key, bytes); return bytes; },
  };
  let inflated = 0;
  const codec = {
    sourceInit() { inflated = 0; },
    charge() {},
    sourceStep(input, eof, capacity) { const take = Math.min(input.length, capacity); inflated += take; return { consumed: take, written: take, data: input.slice(0, take), tag: 40, flags: 2, b: BigInt(inflated) }; },
  };
  const source = new ObjectSource(store, codec, "pin", { ownedBuffers: true });
  source.chunks.limit = PAGE_BYTES;
  const row = { stage: "pin", node: 12, end_offset: 12 + PAGE_BYTES, packed_size: PAGE_BYTES };
  assert.deepEqual(await source.encoded(row, 0, 1), new Uint8Array([0]));
  source.chunks.put("other", new Uint8Array(PAGE_BYTES));
  assert.equal(bytes.byteLength, 0, "the original stored chunk must actually be retired");
  assert.deepEqual(await source.encoded(row, 1, 1), new Uint8Array([1]));
  const pinned = source.reader.input;
  source.releaseCaches(); assert.equal(pinned.byteLength, 0); assert.equal(source.chunks.size, 0);
});
