import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { source, git, refs, receive, compare } from "./browse-fixture.mjs";
import { pack, receiveReport } from "./fixture.mjs";
import { ReadApi } from "../src/browse.mjs";
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));

for (const format of [0, 1]) test(`bounded browse matches real Git and revokes unreachable objects format=${format}`, { timeout: 60000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-browse-")), database = new DatabaseSync(":memory:"), objects = new Map();
  const BLOBS = { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); } };
  const host = new OwnRepository(sqliteContext(database), { AUTH_MODE: "gateway", GRANT_KEY, BLOBS });
  function call(path, method = "GET", body, access = "read") {
    return host.fetch(new Request(`http://host/browse.git/${path}`, { method, headers: { "x-git-zero-namespace": "browse", "x-git-zero-grant": mint("browse", access), "content-type": path === "git-receive-pack" ? "application/x-git-receive-pack-request" : "application/json" }, ...(body === undefined ? {} : { body }) }));
  }
  async function get(path) { const response = await call(`api/v1/${path}`), data = await response.json(); assert.equal(response.status, 200, JSON.stringify(data)); return data; }
  async function push(body) { const response = await call("git-receive-pack", "POST", body, "write"); assert.equal(response.status, 200, await response.clone().text()); const result = receiveReport(await response.arrayBuffer()); assert.ok(!result.includes("ng "), result); }
  try {
    assert.equal((await call("api/v1/refs")).status, 404);
    assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }), "admin")).status, 201);
    assert.deepEqual((await get("refs")).refs, []); assert.deepEqual((await get("history")).commits, []);
    assert.equal((await call("api/v1/commit")).status, 404);
    const fixture = await source(root, format), all = refs(fixture), zero = "0".repeat(format ? 64 : 40);
    await push(receive(format, all.map((ref) => ({ name: ref.name, old: zero, new: ref.oid })), git(fixture.directory, ["pack-objects", "--stdout", "--all", "--delta-base-offset"])));
    await assert.rejects(new ReadApi(host.store, module, format, { visibleObjects: 1 }).handle("commit", new Request(`http://host/browse.git/api/v1/commit?rev=oid:${fixture.first}`)), (error) => error.status === 503 && /configured budget/.test(error.message));
    // A larger deployment budget resumes the same persisted build.
    await compare(fixture, get);
    // Preserve historical bytes using Git's literal-object fixture writer.
    // Existing host policy accepts these with warnings; ls-tree normalizes them.
    const payload = git(fixture.directory, ["rev-parse", "HEAD:payload.html"]).toString().trim();
    const historical = git(fixture.directory, ["hash-object", "--literally", "-t", "tree", "-w", "--stdin"], Buffer.concat([Buffer.from("100664 old-file\0"), Buffer.from(payload, "hex")])).toString().trim();
    await push(receive(format, [{ name: "refs/tags/historical-mode", old: zero, new: historical }], git(fixture.directory, ["pack-objects", "--stdout"], `${historical}\n`)));
    const oldMode = (await get("tree?rev=historical-mode")).entries[0];
    assert.equal(oldMode.mode, git(fixture.directory, ["ls-tree", historical]).toString().slice(0, 6)); assert.equal(oldMode.rawMode, "100664");
    for (const [operation, limits] of [["tree", { treeBytes: 1 }], ["commit", { headerBytes: 32 }], ["commit", { metadataBytes: 1 }], ["commit", { storageBytes: 0 }], ["refs", { jsonBytes: 32 }]]) {
      await assert.rejects(new ReadApi(host.store, module, format, limits).handle(operation, new Request(`http://host/browse.git/api/v1/${operation}`)), (error) => error.status === 413, `${operation} ${JSON.stringify(limits)}`);
    }
    // Global proof caps do not apply to walks rooted in current refs.
    for (const limits of [{ visibleObjects: 1 }, { edges: 1 }]) {
      const response = await new ReadApi(host.store, module, format, limits).handle("commit", new Request("http://host/browse.git/api/v1/commit"));
      assert.equal(response.status, 200);
    }
    const signed = (await get("history?limit=1")).nextCursor;
    const [cursorPayload, signature] = signed.split(".");
    const forged = JSON.parse(Buffer.from(cursorPayload, "base64url")); forged.next = fixture.other;
    assert.equal((await call(`api/v1/history?cursor=${Buffer.from(JSON.stringify(forged)).toString("base64url")}.${signature}`)).status, 400);
    for (const path of ["payload.html", "empty", "link", "src/analysis.py"]) {
      const response = await call(`api/v1/blob?path=${encodeURIComponent(path)}`);
      assert.equal(response.status, 200, await response.clone().text());
      const expected = git(fixture.directory, ["cat-file", "blob", `HEAD:${path}`]);
      assert.equal(response.headers.get("content-type"), "application/octet-stream"); assert.equal(response.headers.get("x-content-type-options"), "nosniff");
      assert.equal(response.headers.get("content-length"), String(expected.length)); assert.equal(response.headers.get("content-disposition"), "attachment");
      assert.deepEqual(Buffer.from(await response.arrayBuffer()), expected);
    }
    assert.equal((await call("api/v1/blob?path=external")).status, 422);
    assert.equal((await call("api/v1/blob?path=src/analysis.py&maxBytes=1")).status, 413);
    assert.equal((await call("api/v1/tree?path=../src")).status, 400);
    assert.equal((await call("api/v1/tree?path=link/nested")).status, 422);
    assert.equal((await call("api/v1/tree?rev=missing")).status, 404);
    assert.equal((await call("api/v1/history?limit=101")).status, 400);
    assert.equal((await call("api/v1/tree?cursor=%%%" )).status, 400);
    await push(receive(format, [{ name: "refs/tags/main", old: zero, new: fixture.first }], pack(format, [])));
    assert.equal((await call("api/v1/commit?rev=main")).status, 409);
    assert.equal((await get(`commit?rev=refs/heads/main`)).oid, fixture.tip);
    const page = await get("history?limit=1"); assert.ok(page.nextCursor);
    const oldRefs = [...all, { name: "refs/tags/main", oid: fixture.first }];
    await push(receive(format, oldRefs.filter((ref) => ref.name !== "refs/heads/other").map((ref) => ({ name: ref.name, old: ref.oid, new: zero })), pack(format, [])));
    await push(receive(format, [{ name: "refs/heads/main", old: zero, new: fixture.other }], pack(format, [])));
    assert.equal((await call(`api/v1/history?cursor=${page.nextCursor}`)).status, 409);
    assert.ok(host.store.published(fixture.tip), "old published bytes are deliberately still stored");
    assert.equal((await call(`api/v1/commit?rev=${fixture.tip}`)).status, 404);
    assert.equal((await call(`api/v1/blob?oid=${fixture.secret}`)).status, 404);
    assert.deepEqual((await get("history")).commits.map((entry) => entry.oid), [fixture.other]);
    await host.queue;
  } finally { database.close(); await rm(root, { recursive: true, force: true }); }
});
