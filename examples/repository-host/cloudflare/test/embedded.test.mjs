import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { source } from "./browse-fixture.mjs";
import { start } from "./wrangler.mjs";
import { GRANT_KEY } from "./grant-fixture.mjs";
const basic = (user) => `Basic ${Buffer.from(`${user}:${user}-development`).toString("base64")}`;
async function git(directory, user, ...args) {
  const child = spawn("git", ["-c", "user.name=Embedded", "-c", "user.email=embedded@example.test", "-c", `http.extraHeader=Authorization: ${basic(user)}`, ...args], { cwd: directory, timeout: 30000, killSignal: "SIGKILL", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  let stdout = "", stderr = ""; child.stdout.on("data", (v) => { stdout += v; }); child.stderr.on("data", (v) => { stderr += v; });
  const status = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); }); return { status, stdout: stdout.trim(), stderr };
}
async function ok(directory, ...args) { const result = await git(directory, "owner", ...args); assert.equal(result.status, 0, result.stderr); return result.stdout; }
test("embedded renamed Durable Object serves direct-stub Git/API calls and retires only its shared-bucket prefix", { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-embedded-")), config = join(directory, "wrangler.toml"); let server;
  const fixture = fileURLToPath(new URL("./fixtures/embedded.mjs", import.meta.url));
  await writeFile(config, `name = "git-zero-embedded-fixture"\nmain = ${JSON.stringify(fixture)}\ncompatibility_date = "2026-10-06"\nsend_metrics = false\n[vars]\nPRODUCT_KEY = "${GRANT_KEY}"\n[[r2_buckets]]\nbinding = "PRODUCT_BUCKET"\nbucket_name = "shared-product-test"\n[[durable_objects.bindings]]\nname = "PRODUCT_REPOS"\nclass_name = "ProductRepository"\n[[migrations]]\ntag = "v1"\nnew_sqlite_classes = ["ProductRepository"]\n`);
  const owner = { authorization: basic("owner") }, reader = { authorization: basic("reader") };
  try {
    server = await start(join(directory, "state"), { config, hostToken: null });
    assert.equal(await (await fetch(`${server.origin}/health`)).text(), "product health");
    const foreign = await fetch(`${server.origin}/_fixture/foreign`, { method: "PUT", headers: owner }); assert.equal(foreign.status, 200); await foreign.arrayBuffer();
    for (const format of [0, 1]) {
      const name = format ? "sha256" : "sha1", remote = `${server.origin}/alpha/analysis-${name}.git`, sourceRepo = await source(directory, format);
      const bytes = Buffer.alloc(200000); let state = 123;
      for (let i = 0; i < bytes.length; i++) { state ^= state << 13; state ^= state >>> 17; state ^= state << 5; bytes[i] = state & 255; }
      await writeFile(join(sourceRepo.directory, "binary"), bytes); await ok(sourceRepo.directory, "add", "binary"); await ok(sourceRepo.directory, "commit", "-m", "binary framework asset");
      const tip = await ok(sourceRepo.directory, "rev-parse", "HEAD");
      const created = await fetch(`${remote}/api/v1/repository`, { method: "PUT", headers: { ...owner, "content-type": "application/json", "x-git-zero-grant": "client-supplied-must-be-replaced" }, body: JSON.stringify({ objectFormat: name }) }); assert.equal(created.status, 201, await created.clone().text()); await created.arrayBuffer();
      await ok(sourceRepo.directory, "-c", "core.compression=0", "push", "--mirror", remote);
      const clone = join(directory, `clone-${format}`), cloned = await git(directory, "reader", "-c", "protocol.version=2", "clone", remote, clone); assert.equal(cloned.status, 0, cloned.stderr);
      assert.equal(await ok(clone, "rev-parse", "HEAD"), tip); await ok(clone, "fsck", "--strict");
      await ok(clone, "fetch", "origin");
      const denied = await git(sourceRepo.directory, "reader", "push", remote, "HEAD:refs/heads/denied"); assert.notEqual(denied.status, 0); assert.match(denied.stderr, /write access required/);
      const body = await fetch(`${remote}/api/v1/blob?path=binary`, { headers: reader }); assert.equal(body.status, 200); assert.equal(body.headers.get("content-length"), String(bytes.length)); assert.equal(body.headers.get("x-content-type-options"), "nosniff"); assert.deepEqual(Buffer.from(await body.arrayBuffer()), bytes);
      const commit = await (await fetch(`${remote}/api/v1/commit?via=handler`, { headers: reader })).json(); assert.equal(commit.oid, tip);
      const record = await (await fetch(`${remote}/api/v1/changes`, { headers: reader })).json(); assert.equal(record.publications[0].actor, "owner");
      const inventory = await (await fetch(`${server.origin}/_fixture/storage`, { headers: owner })).json();
      const chunks = inventory.filter(({ key }) => key.includes(`/repositories/550e8400-e29b-41d4-a716-44665544000${format + 1}/chunks/`)); assert.ok(chunks.length >= 3); assert.ok(chunks.every(({ size }) => size <= 65536));
      assert.equal((await fetch(`${server.origin}/_fixture/wrong-stub`, { method: "PUT", headers: { ...owner, "content-type": "application/json" }, body: "{}" })).status, 404);
      const outsider = await fetch(`${remote}/info/refs?service=git-upload-pack`, { headers: { authorization: basic("outsider") } }); const missing = await fetch(`${server.origin}/alpha/missing.git/info/refs?service=git-upload-pack`, { headers: { authorization: basic("outsider") } }); assert.equal(outsider.status, 404); assert.equal(await outsider.text(), await missing.text());
      let done; for (let i = 0; i < 20; i++) { const response = await fetch(`${remote}/api/v1/repository`, { method: "DELETE", headers: owner }); assert.ok([200, 202].includes(response.status)); done = await response.json(); if (done.complete) break; } assert.equal(done.complete, true);
      const survivors = await (await fetch(`${server.origin}/_fixture/foreign`, { headers: owner })).json(); assert.ok(survivors.every(({ value }) => value === "foreign survives"));
      const left = await (await fetch(`${server.origin}/_fixture/storage`, { headers: owner })).json(); assert.equal(left.some(({ key }) => key.startsWith(`product/git/repositories/550e8400-e29b-41d4-a716-44665544000${format + 1}/`)), false);
      await server.stop(); server = await start(join(directory, "state"), { config, hostToken: null });
      const retired = await fetch(`${server.origin}/alpha/analysis-${name}.git/api/v1/repository`, { method: "PUT", headers: { ...owner, "content-type": "application/json" }, body: JSON.stringify({ objectFormat: name }) }); assert.equal(retired.status, 410); await retired.arrayBuffer();
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
