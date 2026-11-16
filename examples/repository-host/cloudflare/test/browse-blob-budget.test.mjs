import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { readFile } from "node:fs/promises";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { object, entry, pack, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
for (const format of [0, 1]) test(`raised blob limits serve beyond 32 MiB and lowered codec quotas fail before headers format=${format}`, { timeout: 60000 }, async () => {
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), data = new Map();
  const env = { K: GRANT_KEY, B: { async get(key) { const value = data.get(key); return value ? { size: value.length, async arrayBuffer() { return value.slice().buffer; } } : null; }, async put(key, value) { data.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) data.delete(key); } } };
  const options = { codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "", readLimits: { blobBytes: 64 * 1024 * 1024 } };
  const factory = createRepositoryHost(options); let host = new factory.Repository(ctx, env);
  const call = (route, method = "GET", body, type = "application/json") => host.fetch(new Request(`http://host/blob.git/${route}`, { method, headers: { "x-git-zero-namespace": "blob", "x-git-zero-grant": mint("blob", "admin"), "content-type": type }, ...(body === undefined ? {} : { body }) }));
  try {
    assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }))).status, 201);
    const value = object(format, 3, Buffer.alloc(33 * 1024 * 1024, 97));
    const pushed = await call("git-receive-pack", "POST", receive(format, [{ name: "refs/tags/payload", old: "0".repeat(format ? 64 : 40), new: value.id.toString("hex") }], pack(format, [entry(3, value.body)])), "application/x-git-receive-pack-request");
    assert.equal(pushed.status, 200); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /); await host.queue;
    const before = db.prepare("SELECT total_changes() n").get().n;
    const response = await call("api/v1/blob?rev=payload"); assert.equal(response.status, 200);
    assert.equal(response.headers.get("content-length"), String(value.body.length));
    const reader = response.body.getReader(); let length = 0;
    for (;;) { const next = await reader.read(); if (next.done) break; assert.ok(next.value.every((byte) => byte === 97)); length += next.value.length; }
    assert.equal(length, value.body.length); await host.queue;
    assert.equal(db.prepare("SELECT total_changes() n").get().n, before, "even large ref-rooted blob reads write no rows");
    for (const codecLimits of [{ maxCanonicalBytes: 1 }, { maxObjectBytes: 1 }]) {
      const constrained = createRepositoryHost({ ...options, codecLimits }); host = new constrained.Repository(ctx, env);
      const refused = await call("api/v1/blob?rev=payload"); assert.equal(refused.status, 413, "a known output quota failure precedes streamed headers");
      assert.match(refused.headers.get("content-type"), /application\/json/); assert.equal(refused.headers.has("x-object-id"), false);
      assert.match(await refused.text(), /budget/i); await host.queue;
    }
  } finally { await host.queue; db.close(); }
});
