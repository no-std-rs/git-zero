import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { createServer } from "node:http";
import { once } from "node:events";
import { Readable } from "node:stream";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { tmpdir } from "node:os";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { object, entry, pack, receiveReport, wire, packet } from "./fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const SIZE = 9 * 1024 * 1024, TOKEN = "public-fetch-quota-test";
function variable(value) { const out = []; do { const b = value & 127; value = Math.floor(value / 128); out.push(b | (value ? 128 : 0)); } while (value); return Buffer.from(out); }
for (const format of [0, 1]) test(`real Git fetch ignores stored quota and pending accounting, preflights fallback format=${format}`, { timeout: 60000 }, async () => {
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), data = new Map(), dir = await mkdtemp(join(tmpdir(), "git-zero-fetch-quota-")); let host, observedScratch = false;
  const env = { TOKEN, BLOBS: { async get(key) { const value = data.get(key); return value ? { size: value.length, async arrayBuffer() { return value.slice().buffer; } } : null; }, async put(key, value) { data.set(key, new Uint8Array(value).slice()); if (key.includes("/tmp/") && host?.store?.exec("SELECT 1 FROM read_scratch WHERE key=?", key).length) observedScratch = true; }, async delete(keys) { for (const key of keys) data.delete(key); }, async list({ prefix, limit }) { const keys = [...data.keys()].filter((key) => key.startsWith(prefix)).sort(); return { objects: keys.slice(0, limit).map((key) => ({ key })), truncated: keys.length > limit }; } } };
  function instance(codecLimits = {}, extra = {}) { const factory = createRepositoryHost({ codec, authMode: "token", bindings: { repositories: "REPOS", objects: "BLOBS", token: "TOKEN" }, routePrefix: "", keyPrefix: "", codecLimits, ...extra }); return new factory.Repository(ctx, env); }
  host = instance();
  const call = (route, method = "GET", body, type = "application/json") => host.fetch(new Request(`http://host/quota.git/${route}`, { method, headers: { authorization: `Bearer ${TOKEN}`, "x-git-zero-namespace": "quota", "content-type": type }, ...(body === undefined ? {} : { body }) }));
  const server = createServer(async (request, response) => {
    try {
      const target = new Request(`http://host${request.url}`, { method: request.method, headers: { ...request.headers, "x-git-zero-namespace": "quota" }, ...(["GET", "HEAD"].includes(request.method) ? {} : { body: Readable.toWeb(request), duplex: "half" }) });
      const result = await host.fetch(target); response.writeHead(result.status, Object.fromEntries(result.headers));
      if (result.body) Readable.fromWeb(result.body).on("error", (error) => response.destroy(error)).pipe(response); else response.end();
    } catch (error) { response.writeHead(500); response.end(String(error)); }
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  async function git(args) {
    const { spawn } = await import("node:child_process");
    const child = spawn("git", ["-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, ...args], { cwd: dir, timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
    let stdout = "", stderr = ""; child.stdout.on("data", (p) => { stdout += p; }); child.stderr.on("data", (p) => { stderr += p; });
    const [status] = await once(child, "exit"); assert.equal(status, 0, stderr); return stdout;
  }
  try {
    assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }))).status, 201);
    const base = object(format, 3, Buffer.alloc(SIZE, 97)), target = object(format, 3, Buffer.concat([base.body, Buffer.from("!")]));
    const delta = Buffer.concat([variable(SIZE), variable(SIZE + 1), Buffer.from([0xf0, SIZE & 255, (SIZE >>> 8) & 255, (SIZE >>> 16) & 255, 1, 33])]);
    for (const [packed, obj, name] of [[pack(format, [entry(3, base.body)]), base, "refs/tags/base"], [pack(format, [entry(7, delta, base.id)]), target, "refs/tags/large"]]) {
      const response = await call("git-receive-pack", "POST", wire(format, packed, obj.id, name), "application/x-git-receive-pack-request"); assert.equal(response.status, 200, await response.clone().text()); assert.doesNotMatch(receiveReport(await response.arrayBuffer()), /ng /); await host.queue;
    }
    assert.equal((await call("api/v1/policy", "PATCH", JSON.stringify({ maxStoredBytes: 0 }))).status, 200);
    for (const pending of [false, true]) {
      if (pending) {
        await host.queue; db.exec("DROP TABLE host_chunk_sizes; DROP TABLE host_storage_usage"); host = instance();
        // The cold object reconstructs an additive, still-pending legacy ledger.
      }
      const client = `client-${pending}.git`;
      await git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, client]);
      await git(["-C", client, "-c", "protocol.version=0", "fetch", "--quiet", "--no-tags", `http://127.0.0.1:${server.address().port}/quota.git`, "refs/tags/large:refs/tags/large"]);
      assert.equal((await git(["-C", client, "cat-file", "-s", "refs/tags/large"])).trim(), String(SIZE + 1));
      assert.equal((await git(["-C", client, "rev-parse", "refs/tags/large"])).trim(), target.id.toString("hex"));
      await host.queue;
      assert.equal(host.store.exec("SELECT COUNT(*) n FROM read_scratch")[0].n, 0);
      assert.equal(host.store.exec("SELECT bytes FROM read_scratch_usage")[0].bytes, 0);
      assert.equal(host.store.exec("SELECT COUNT(*) n FROM temp_chunks")[0].n, 0);
      assert.equal(host.store.storageAccounting().ready, !pending, "fetch never forces or blocks on write accounting");
    }
    assert.equal(observedScratch, true, "the large fallback must exercise exempt temporary chunks");
    host = instance({ maxTemporaryBytes: 1 });
    const body = Buffer.concat([packet(`want ${target.id.toString("hex")} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]);
    const refused = await call("git-upload-pack", "POST", body, "application/x-git-upload-pack-request");
    assert.notEqual(refused.status, 200, "known reconstruction failure must precede streamed headers");
    assert.match(await refused.text(), /budget|codec/i); await host.queue;
    assert.equal(host.store.exec("SELECT COUNT(*) n FROM temp_chunks")[0].n, 0);
    host = instance({}, { readScratchRepositoryBytes: 1 });
    const scratch = await call("git-upload-pack", "POST", body, "application/x-git-upload-pack-request");
    assert.equal(scratch.status, 503); assert.equal(scratch.headers.get("retry-after"), "1"); assert.match(await scratch.text(), /read scratch budget/); await host.queue;
    assert.equal(host.store.exec("SELECT bytes FROM read_scratch_usage")[0].bytes, 0);
    host = instance({}, { fetchPrepareMs: 1 });
    const timed = await call("git-upload-pack", "POST", body, "application/x-git-upload-pack-request");
    assert.equal(timed.status, 504); assert.match(await timed.text(), /preparation.*deadline/); await host.queue;
    host = instance();
    const active = await call("git-upload-pack", "POST", body, "application/x-git-upload-pack-request");
    assert.equal(active.status, 200);
    const deletedFirst = await call("api/v1/repository", "DELETE");
    assert.equal(deletedFirst.status, 202, "DELETE fences immediately while live I/O settles");
    await assert.rejects(active.arrayBuffer(), /deleted|cancelled/);
    await host.queue;
    let deleted = deletedFirst;
    while (deleted.status === 202) { await deleted.arrayBuffer(); deleted = await call("api/v1/repository", "DELETE"); }
    assert.equal(deleted.status, 200, await deleted.clone().text()); await deleted.arrayBuffer(); await host.queue;
    assert.equal([...data.keys()].some((key) => key.startsWith("repositories/quota/")), false);
  } finally { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); await host.queue; db.close(); await rm(dir, { recursive: true, force: true }); }
});
