import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { headers, TOKEN } from "./wrangler.mjs";
import { source, git, compare } from "./browse-fixture.mjs";
import { spawn } from "node:child_process";

async function push(directory, remote) {
  const child = spawn("git", ["-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, "push", "--mirror", remote], { cwd: directory, timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  let errors = ""; child.stderr.on("data", (data) => { errors += data; }); child.stdout.resume();
  const status = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
  assert.equal(status, 0, errors);
}

testRuntimes("{runtime} serves bounded browse metadata and file bytes compared with real Git in both formats", { timeout: 120000 }, async (serve) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-browse-runtime-")); let server;
  try {
    server = await serve(join(root, "state"));
    for (const format of [0, 1]) {
      const fixture = await source(root, format), remote = `${server.origin}/browse-${format}.git`;
      const created = await fetch(`${remote}/api/v1/repository`, { method: "PUT", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }) });
      assert.equal(created.status, 201, await created.clone().text()); await created.arrayBuffer();
      // Native Git is the client as well as the independent metadata oracle.
      await push(fixture.directory, remote);
      const get = async (path) => { const response = await fetch(`${remote}/api/v1/${path}`, { headers: headers() }), value = await response.json(); assert.equal(response.status, 200, JSON.stringify(value)); return value; };
      await compare(fixture, get);
      for (const path of ["src/analysis.py", "payload.html", "empty", "link"]) {
        const response = await fetch(`${remote}/api/v1/blob?path=${encodeURIComponent(path)}`, { headers: headers() });
        const expected = git(fixture.directory, ["cat-file", "blob", `HEAD:${path}`]);
        assert.equal(response.status, 200); assert.equal(response.headers.get("content-length"), String(expected.length));
        assert.equal(response.headers.get("content-type"), "application/octet-stream"); assert.equal(response.headers.get("x-content-type-options"), "nosniff");
        assert.deepEqual(Buffer.from(await response.arrayBuffer()), expected);
      }
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
