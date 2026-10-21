import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { packet, pack, project, receiveReport } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

const execute = promisify(execFile);
async function git(...args) {
  const { stdout } = await execute("git", ["-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, ...args], {
    timeout: 30000, killSignal: "SIGKILL", encoding: "utf8",
    env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" },
  });
  return stdout.trim();
}
function request(format, old, value, name) {
  const caps = `report-status-v2 delete-refs object-format=${format ? "sha256" : "sha1"}`;
  return Buffer.concat([packet(`${old} ${value} ${name}\0${caps}\n`), Buffer.from("0000"), /^0+$/.test(value) ? Buffer.alloc(0) : pack(format, [])]);
}

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} clones a surviving branch after deleting HEAD`, { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-head-"));
  let server;
  try {
    const state = join(directory, "state");
    server = await serve(state);
    for (const format of [0, 1]) {
      const namespace = `head-${format}`, fixture = project(format), id = fixture.root.id.toString("hex"), zero = "0".repeat(id.length);
      const remote = `${server.origin}/${namespace}.git`;
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const send = async (body) => {
        const response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body });
        assert.equal(response.status, 200); const bytes = Buffer.from(await response.arrayBuffer()); assert.ok(!bytes.includes(Buffer.from("unpack invalid")), bytes.toString()); return bytes;
      };
      assert.match(receiveReport(await send(fixture.wire)), /ok refs\/heads\/main/);
      for (const name of ["a", "master"]) assert.ok((await send(request(format, zero, id, `refs/heads/${name}`))).includes(Buffer.from(`ok refs/heads/${name}`)));
      assert.ok((await send(request(format, id, zero, "refs/heads/main"))).includes(Buffer.from("ok refs/heads/main")));
      for (const protocol of [0, 1, 2]) {
        const clone = join(directory, `clone-${format}-${protocol}`);
        await git("-c", `protocol.version=${protocol}`, "clone", remote, clone);
        assert.equal(await git("-C", clone, "symbolic-ref", "--short", "HEAD"), "master");
        assert.equal(await git("-C", clone, "rev-parse", "HEAD"), id);
        await git("-C", clone, "fsck", "--full", "--strict");
      }
      assert.ok((await send(request(format, id, zero, "refs/heads/master"))).includes(Buffer.from("ok refs/heads/master")));
      assert.match(await git("ls-remote", "--symref", remote, "HEAD"), /ref: refs\/heads\/a\s+HEAD/);
    }
    await server.stop(); server = await serve(state);
    for (const format of [0, 1]) assert.match(await git("ls-remote", "--symref", `${server.origin}/head-${format}.git`, "HEAD"), /ref: refs\/heads\/a\s+HEAD/);
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
