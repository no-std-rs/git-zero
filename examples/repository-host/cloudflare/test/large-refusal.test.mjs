import assert from "node:assert/strict";
import { testRuntimes } from "./runtimes.mjs";
import { createServer } from "node:http";
import { once } from "node:events";
import { Readable } from "node:stream";
import { createGzip } from "node:zlib";
import { randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { headers, TOKEN } from "./wrangler.mjs";
async function git(dir, args) {
  const child = spawn("git", ["-c", "user.name=Test", "-c", "user.email=t@example.test", "-c", "core.compression=0", "-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, ...args], { cwd: dir, timeout: 90000, killSignal: "SIGKILL", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  let stdout = "", stderr = ""; child.stdout.on("data", (p) => { stdout += p; }); child.stderr.on("data", (p) => { stderr += p; }); const [status] = await once(child, "exit"); return { status, stdout, stderr };
}
testRuntimes("{runtime} real Git sees a size refusal for a 32 MiB pack, including gzip transport", { timeout: 240000 }, async (serve) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-large-refusal-")); let server, proxy;
  try {
    server = await serve(join(root, "state"));
    proxy = createServer(async (req, res) => {
      try {
        const outgoing = new Headers(req.headers), gzip = req.method === "POST" && req.url.endsWith("/git-receive-pack");
        outgoing.delete("host"); outgoing.delete("content-length"); outgoing.delete("transfer-encoding"); if (gzip) outgoing.set("content-encoding", "gzip");
        const response = await fetch(server.origin + req.url, { method: req.method, headers: outgoing, ...(["GET", "HEAD"].includes(req.method) ? {} : { body: Readable.toWeb(gzip ? req.pipe(createGzip()) : req), duplex: "half" }) });
        res.writeHead(response.status, Object.fromEntries(response.headers)); if (response.body) Readable.fromWeb(response.body).on("error", (e) => res.destroy(e)).pipe(res); else res.end();
      } catch (error) { res.writeHead(502); res.end(String(error)); }
    });
    proxy.listen(0, "127.0.0.1"); await once(proxy, "listening");
    const payload = randomBytes(32 * 1024 * 1024);
    for (const format of [0, 1]) {
      const dir = join(root, `client-${format}`); await mkdir(dir);
      assert.equal((await git(dir, ["init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`])).status, 0);
      await writeFile(join(dir, "large.bin"), payload); assert.equal((await git(dir, ["add", "."])).status, 0); assert.equal((await git(dir, ["commit", "-m", "large upload"])).status, 0);
      const oid = (await git(dir, ["rev-parse", "HEAD:large.bin"])).stdout.trim();
      for (const gzip of [false, true]) {
        const ns = `large-${format}-${gzip}`, remote = `${gzip ? `http://127.0.0.1:${proxy.address().port}` : server.origin}/${ns}.git`;
        const created = await fetch(`${server.origin}/${ns}.git/api/v1/repository`, { method: "PUT", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }) }); assert.equal(created.status, 201); await created.arrayBuffer();
        const patch = await fetch(`${server.origin}/${ns}.git/api/v1/policy`, { method: "PATCH", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify({ maxPushBytes: 1024 }) }); assert.equal(patch.status, 200); await patch.arrayBuffer();
        const result = await git(dir, ["push", "--porcelain", remote, "main"]);
        assert.notEqual(result.status, 0); assert.match(result.stderr, /single-push size limit exceeded/, JSON.stringify({ gzip, result })); assert.doesNotMatch(result.stderr, /broken pipe|RPC failed|unexpected disconnect/i);
        const state = await (await fetch(`${server.origin}/${ns}.git/api/v1/repository`, { headers: headers() })).json(); assert.equal(state.generation, 0); assert.equal(state.objectCount, 0);
        assert.equal((await fetch(`${server.origin}/${ns}.git/api/v1/blob?oid=${oid}`, { headers: headers() })).status, 404, "rejected raw objects remain hidden");
        assert.equal((await fetch(`${server.origin}/${ns}.git/api/v1/commit?rev=oid:${(await git(dir, ["rev-parse", "HEAD"])).stdout.trim()}`, { headers: headers() })).status, 404);
      }
    }
  } finally { if (proxy) { proxy.closeAllConnections(); await new Promise((resolve) => proxy.close(resolve)); } await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
