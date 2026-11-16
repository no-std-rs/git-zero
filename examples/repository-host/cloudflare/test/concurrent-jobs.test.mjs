import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, packet, pack, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));

test("same-repository fetches own independent scratch and publication does not wait for them", { timeout: 10000 }, async () => {
  const factory = createRepositoryHost({ codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "", packCacheEnabled: false,
    memory: { canonicalCacheBytes: 0, representationCacheBytes: 0, chunkCacheBytes: 0, canonicalCaptureBytes: 65536, representationCaptureBytes: 65536, chunkBytes: 65536 },
    codecLimits: { maxControlBytes: 1024 * 1024 }, readLimits: { metadataBytes: 1024 * 1024, jsonBytes: 128 * 1024 }, readPinBytes: 1024 * 1024 });
  const db = new DatabaseSync(":memory:"), objects = new Map(), ctx = sqliteContext(db);
  const host = new factory.Repository(ctx, { K: GRANT_KEY, B: { async get(key) { const value = objects.get(key); return value ? { size: value.length, async arrayBuffer() { return value.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); } } });
  const call = (route, method = "GET", body, access = "read") => host.fetch(new Request(`http://host/repo.git/${route}`, { method, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", access), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, ...(body === undefined ? {} : { body }) }));
  const data = project(0), tip = data.root.id.toString("hex"), zero = "0".repeat(40), held = [];
  async function push(name, old = zero, bytes = pack(0, [])) { const response = await call("git-receive-pack", "POST", receive(0, [{ name, old, new: tip }], bytes), "write"); assert.equal(response.status, 200); return receiveReport(await response.arrayBuffer()); }
  try {
    assert.equal((await call("api/v1/repository", "PUT", "{}", "admin")).status, 201);
    assert.match(await push("refs/heads/main", zero, data.pack), /ok refs\/heads\/main/);
    const controls = Buffer.concat([packet(`want ${tip} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]);
    held.push(...await Promise.all([call("git-upload-pack", "POST", controls), call("git-upload-pack", "POST", controls)]));
    for (const response of held) assert.equal(response.status, 200);
    assert.equal(db.prepare("SELECT COUNT(*) n FROM sqlite_master WHERE type='table' AND name GLOB 'job_*_fetch_selected'").get().n, 2);
    const browse = await Promise.race([call("api/v1/commit"), delay(1000).then(() => { throw new Error("browse waited for clone EOF"); })]);
    assert.equal(browse.status, 200); assert.equal((await browse.json()).oid, tip);
    assert.match(await Promise.race([push("refs/tags/during"), delay(1000).then(() => { throw new Error("push waited for clone EOF"); })]), /ok refs\/tags\/during/);
    const reports = await Promise.all([push("refs/tags/a"), push("refs/tags/b")]); assert.ok(reports.every((value) => value.includes("ok ")));
    const race = await Promise.all([push("refs/tags/race"), push("refs/tags/race")]);
    assert.equal(race.filter((value) => value.includes("ok ")).length, 1); assert.equal(race.filter((value) => /ng .*stale ref/.test(value)).length, 1);
    for (const response of held) assert.ok((await response.arrayBuffer()).byteLength > 0); held.length = 0;
    await host.queue; assert.equal(host.store.fetchActive.size, 0);
    for (const { name } of db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB 'job_*_fetch_*'").all()) assert.equal(db.prepare(`SELECT COUNT(*) n FROM ${name}`).get().n, 0);
  } finally { for (const response of held) await response.body.cancel().catch(() => {}); await host.queue; db.close(); }
});
