import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { gzipSync } from "node:zlib";
import { start } from "./wrangler.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { packet, project, wire, receiveReport } from "./fixture.mjs";
const root = fileURLToPath(new URL("../", import.meta.url));
const basic = (user) => `Basic ${Buffer.from(`${user}:${user}-development`).toString("base64")}`;
async function git(cwd, auth, ...args) {
  const child = spawn("git", ["-c", "user.name=Gateway", "-c", "user.email=gateway@example.test", "-c", `http.extraHeader=${auth}`, ...args], { cwd, timeout: 30000, killSignal: "SIGKILL", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  let stdout = "", stderr = "";
  child.stdout.on("data", (b) => { stdout += b; }); child.stderr.on("data", (b) => { stderr += b; });
  const status = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
  return { status, stdout: stdout.trim(), stderr };
}
async function ok(cwd, auth, ...args) { const result = await git(cwd, auth, ...args); assert.equal(result.status, 0, `${args.join(" ")}\n${result.stderr}`); return result.stdout; }

for (const binding of [true, false]) test(`gateway grants over ${binding ? "service binding" : "plain HTTP"} with real Git`, { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-gateway-")); let server;
  try {
    const hostConfig = join(directory, "host.toml"), gatewayConfig = join(directory, "gateway.toml");
    let config = await readFile(join(root, "wrangler.toml"), "utf8");
    config = config.replace('main = "src/streamed.mjs"', `main = ${JSON.stringify(join(root, "src/streamed.mjs"))}`).replace('[build]\ncommand = "npm run build:streamed"\nwatch_dir = ["src", "../wasm/src"]', '').replace('[vars]', `[vars]\nAUTH_MODE = "gateway"\nGRANT_KEY = "${GRANT_KEY}"`);
    await writeFile(hostConfig, config);
    await writeFile(gatewayConfig, `name = "git-zero-gateway-fixture"\nmain = ${JSON.stringify(join(root, "test/fixtures/gateway.mjs"))}\ncompatibility_date = "2026-10-06"\nsend_metrics = false\n[vars]\nGRANT_KEY = "${GRANT_KEY}"\n[[services]]\nbinding = "HOST"\nservice = "git-zero-streamed-repository-host"\n`);
    server = await start(join(directory, "state"), { config: binding ? [gatewayConfig, hostConfig] : hostConfig, hostToken: null });
    const ns = binding ? "YWxwaGEvYW5hbHlzaXM" : "550e8400-e29b-41d4-a716-446655440000";
    const remote = `${server.origin}/${binding ? "alpha/analysis" : ns}.git`;
    const auth = (level) => binding ? `Authorization: ${basic(level === "read" ? "reader" : "owner")}` : `X-Git-Zero-Grant: ${mint(ns, level)}`;
    const source = join(directory, "source"); await mkdir(source);
    await ok(source, "", "init", "--initial-branch=main");
    await writeFile(join(source, "file.py"), "print('framework')\n");
    await ok(source, "", "add", "."); await ok(source, "", "commit", "-m", "initial");
    // Milestone 1 bootstrap: an admin may initialize the existing implicit path.
    await ok(source, auth("admin"), "ls-remote", remote);
    await ok(source, auth("write"), "push", remote, "main");
    const first = await ok(source, "", "rev-parse", "HEAD");
    for (const version of [0, 1, 2]) {
      const clone = join(directory, `clone-${version}`);
      await ok(directory, auth("read"), "-c", `protocol.version=${version}`, "clone", remote, clone);
      assert.equal(await ok(clone, "", "rev-parse", "HEAD"), first);
      const denied = await git(source, auth("read"), "-c", `protocol.version=${version}`, "push", remote, "HEAD:refs/heads/denied");
      assert.notEqual(denied.status, 0); assert.match(denied.stderr, /write access required/);
      // A valid gzip upload control is passed through without re-encoding.
      const body = version === 2 ? Buffer.concat([packet("command=ls-refs\n"), Buffer.from("00010000")]) : Buffer.concat([packet(`want ${first}\n`), Buffer.from("0000"), packet("done\n")]);
      const headers = { "content-type": "application/x-git-upload-pack-request", "content-encoding": "gzip", "git-protocol": `version=${version}`, ...(binding ? { authorization: basic("reader") } : { "x-git-zero-grant": mint(ns) }) };
      const response = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers, body: gzipSync(body) });
      assert.equal(response.status, 200, await response.clone().text()); await response.arrayBuffer();
      const fixture = project(0);
      const accepted = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: { ...headers, ...(binding ? { authorization: basic("owner") } : { "x-git-zero-grant": mint(ns, "write") }), "content-type": "application/x-git-receive-pack-request" }, body: gzipSync(wire(0, fixture.pack, fixture.root.id, `refs/tags/gzip-${version}`)) });
      assert.equal(accepted.status, 200);
      assert.match(receiveReport(await accepted.arrayBuffer()), new RegExp(`ok refs/tags/gzip-${version}`));
      const receive = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: { ...headers, "content-type": "application/x-git-receive-pack-request" }, body: gzipSync(Buffer.from("0000")) });
      assert.match(await receive.text(), /ERR write access required/);
    }
    await writeFile(join(source, "file.py"), "print('updated framework')\n"); await ok(source, "", "commit", "-am", "second");
    await ok(source, auth("write"), "push", remote, "main");
    for (const version of [0, 1, 2]) {
      const clone = join(directory, `clone-${version}`);
      await ok(clone, auth("read"), "-c", `protocol.version=${version}`, "fetch", "origin");
      assert.equal(await ok(clone, "", "rev-parse", "origin/main"), await ok(source, "", "rev-parse", "HEAD"));
      await ok(clone, "", "fsck", "--strict");
    }
    const hiddenHeaders = binding ? { authorization: basic("outsider") } : {};
    const denied = await fetch(`${remote}/info/refs?service=git-upload-pack`, { headers: hiddenHeaders });
    const missing = await fetch(`${server.origin}/${binding ? "alpha/missing" : "missing"}.git/info/refs?service=git-upload-pack`, { headers: binding ? hiddenHeaders : { "x-git-zero-grant": mint("missing") } });
    assert.equal(denied.status, 404); assert.equal(missing.status, 404);
    assert.equal(denied.headers.has("www-authenticate"), false); assert.equal(missing.headers.has("www-authenticate"), false);
    assert.equal(await denied.text(), await missing.text());
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
