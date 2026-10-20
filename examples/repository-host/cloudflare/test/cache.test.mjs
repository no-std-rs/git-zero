import assert from "node:assert/strict";
import { test } from "node:test";
import { ByteCache } from "../src/storage.mjs";

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
