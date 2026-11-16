import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, packet, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
test("16 cached fetches share one generation and release all codecs before client EOF", { timeout: 10000 }, async () => {
  const factory = createRepositoryHost({ codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "" });
  const db = new DatabaseSync(":memory:"), objects = new Map(), host = new factory.Repository(sqliteContext(db), { K: GRANT_KEY, B: {
    async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; },
    async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); },
  } });
  const call = (route, method = "GET", body, role = "read") => host.fetch(new Request(`http://host/repo.git/${route}`, { method, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", role), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, ...(body === undefined ? {} : { body }) }));
  const held = [], fixture = project(0), tip = fixture.root.id.toString("hex");
  try {
    assert.equal((await call("api/v1/repository", "PUT", "{}", "admin")).status, 201);
    const pushed = await call("git-receive-pack", "POST", receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: tip }], fixture.pack), "write"); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    const body = Buffer.concat([packet(`want ${tip} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]);
    held.push(...await Promise.all(Array.from({ length: 16 }, () => call("git-upload-pack", "POST", body))));
    for (const response of held) { if (response.status !== 200) assert.fail(await response.text()); }
    const cache = host.packCache; assert.equal(cache.metrics.builds, 1); assert.equal(host.env.BYTE_BUDGET.used, 16 * host.env.CACHE_READER_MEMORY_BYTES);
    assert.equal(host.store.fetchActive.size, 0);
    for (const { name } of db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB 'job_*_fetch_*'").all()) assert.equal(db.prepare(`SELECT COUNT(*) n FROM ${name}`).get().n, 0);
    const payloads = await Promise.all(held.map((response) => response.arrayBuffer())); held.length = 0;
    for (const value of payloads) assert.deepEqual(value, payloads[0]);
    await host.queue; assert.equal(host.env.BYTE_BUDGET.used, 0); assert.equal(cache.pins.size, 0); assert.equal(cache.metrics.served, 16);
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); await host.queue; db.close(); }
});
