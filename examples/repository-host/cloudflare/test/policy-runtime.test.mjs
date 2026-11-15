import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawn } from "node:child_process";
import { source } from "./browse-fixture.mjs";
import { headers, TOKEN } from "./wrangler.mjs";
async function git(directory, ...args) {
  const child = spawn("git", ["-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, ...args], { cwd: directory, timeout: 30000, killSignal: "SIGKILL", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  let stdout = "", stderr = ""; child.stdout.on("data", (v) => { stdout += v; }); child.stderr.on("data", (v) => { stderr += v; });
  const status = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); }); return { status, stdout, stderr };
}
testRuntimes("{runtime} explains policy refusals to real Git and permits configured rewrites in both formats", { timeout: 120000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-policy-runtime-")); let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      const fixture = await source(directory, format), remote = `${server.origin}/policy-${format}.git`;
      const created = await fetch(`${remote}/api/v1/repository`, { method: "PUT", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }) }); assert.equal(created.status, 201); await created.arrayBuffer();
      const patch = async (value) => { const response = await fetch(`${remote}/api/v1/policy`, { method: "PATCH", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify(value) }); assert.equal(response.status, 200, await response.clone().text()); return response.json(); };
      await patch({ maxPushBytes: 1 });
      let result = await git(fixture.directory, "push", remote, "main"); assert.notEqual(result.status, 0); assert.match(result.stderr, /single-push size limit exceeded/);
      await patch({ maxPushBytes: null, maxStoredBytes: 1 });
      result = await git(fixture.directory, "push", remote, "main"); assert.notEqual(result.status, 0); assert.match(result.stderr, /repository stored-size limit exceeded/);
      await patch({ maxStoredBytes: null }); result = await git(fixture.directory, "push", remote, "main"); assert.equal(result.status, 0, result.stderr);
      result = await git(fixture.directory, "push", "--force", remote, "other:main"); assert.notEqual(result.status, 0); assert.match(result.stderr, /non-fast-forward/);
      await patch({ allowNonFastForward: true }); result = await git(fixture.directory, "push", "--force", remote, "other:main"); assert.equal(result.status, 0, result.stderr);
      const commit = await (await fetch(`${remote}/api/v1/commit`, { headers: headers() })).json(); assert.equal(commit.oid, fixture.other);
      assert.equal((await fetch(`${remote}/api/v1/commit?rev=${fixture.tip}`, { headers: headers() })).status, 404, "rewritten history is not browse-readable");
      const state = await (await fetch(`${remote}/api/v1/repository`, { headers: headers() })).json(); assert.ok(state.storedBytes > 0); assert.equal(state.storageAccounting.ready, true);
      await patch({ maxStoredBytes: 0, maxPushBytes: 0 }); result = await git(fixture.directory, "push", remote, ":main"); assert.equal(result.status, 0, result.stderr);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
