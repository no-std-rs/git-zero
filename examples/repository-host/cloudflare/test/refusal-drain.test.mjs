import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { createRepositoryHost } from "../src/host.mjs";
import { refusal } from "../src/body.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
function upload(chunks = 100) {
  let sent = 0, ended = false, cancelled = false;
  return { stream: new ReadableStream({ pull(c) { if (sent === chunks) { ended = true; c.close(); } else { sent++; c.enqueue(new Uint8Array(65536)); } }, cancel() { cancelled = true; } }), state: () => ({ sent, ended, cancelled }) };
}
test("hidden, read-only, mismatched-stub and busy POST refusals drain bounded bodies without parsing", async () => {
  const factory = createRepositoryHost({ codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "", maxRequestsPerRepository: 1 });
  const db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), env = { K: GRANT_KEY, B: { async get() { return null; } }, R: { idFromName: (name) => ({ toString: () => name }), get() { throw new Error("denied entry must not forward"); } } };
  const repo = new factory.Repository({ ...ctx, id: { toString: () => "repo" } }, env);
  const request = (body, grant = mint("repo", "read"), namespace = "repo") => new Request(`http://host/${namespace}.git/git-receive-pack`, { method: "POST", headers: { "x-git-zero-namespace": namespace, "x-git-zero-grant": grant, "content-type": "application/x-git-receive-pack-request", "content-encoding": "gzip" }, body, duplex: "half" });
  let held;
  try {
    for (const [call, grant, namespace, status] of [[(req) => factory.fetch(req, env), "invalid", "repo", 404], [(req) => repo.fetch(req), mint("repo", "read"), "repo", 200], [(req) => repo.fetch(req), mint("other", "write"), "other", 404], [(req) => repo.fetch(req), mint("repo", "write"), "repo", 404]]) {
      const body = upload(), response = await call(request(body.stream, grant, namespace)); assert.equal(response.status, status); assert.equal(body.state().ended, true); await response.arrayBuffer();
      assert.equal(db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().length, 0, "refusal must not initialize a nonexistent repository");
    }
    const headers = { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", "admin"), "content-type": "application/json" };
    assert.equal((await repo.fetch(new Request("http://host/repo.git/api/v1/repository", { method: "PUT", headers, body: "{}" }))).status, 201);
    held = await repo.fetch(new Request("http://host/repo.git/info/refs?service=git-upload-pack", { headers }));
    const body = upload(); const busy = await repo.fetch(request(body.stream, mint("repo", "write"))); assert.equal(busy.status, 503); assert.equal(body.state().ended, true);
    await held.arrayBuffer(); held = null; await repo.queue;
  } finally { await held?.body.cancel().catch(() => {}); await repo.queue; db.close(); }
});
test("refusal discard has byte and wall-clock bounds even for a stalled peer", async () => {
  const large = upload();
  const response = await refusal(new Request("http://host", { method: "POST", body: large.stream, duplex: "half" }), new Response("refused", { status: 404 }), { REJECTION_DRAIN_BYTES: 65536, REJECTION_DRAIN_MS: 1000 });
  assert.equal(response.status, 404); assert.equal(large.state().cancelled, true); assert.ok(large.state().sent <= 3, "at most the bound, crossing chunk and stream lookahead are pulled");
  let cancelled = false;
  const stalled = new ReadableStream({ pull() { return new Promise(() => {}); }, cancel() { cancelled = true; } });
  const before = Date.now(); await refusal(new Request("http://host", { method: "POST", body: stalled, duplex: "half" }), new Response("refused"), { REJECTION_DRAIN_MS: 20 });
  assert.equal(cancelled, true); assert.ok(Date.now() - before < 1000);
});
