import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { packet, project, receiveReport } from "./fixture.mjs";
import { headers, start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

const gitEnv = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" };

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} accepts both acknowledgement modes selected by independent clients`, { timeout: 60000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-dual-ack-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      const fixture = project(format), id = fixture.root.id.toString("hex");
      const remote = `${server.origin}/dual-ack-${format}.git`;
      const initialized = await fetch(`${server.origin}/_test/dual-ack-${format}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const pushed = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: fixture.wire });
      assert.equal(pushed.status, 200); assert.match(receiveReport(await pushed.arrayBuffer()), /ok refs\/heads\/main/);
      const repository = join(directory, `reference-${format}`);
      const initializedGit = spawnSync("git", ["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, repository], { encoding: "utf8", env: gitEnv });
      assert.equal(initializedGit.status, 0, initializedGit.stderr);
      for (const version of [0, 1]) for (const modes of ["multi_ack multi_ack_detailed", "multi_ack_detailed multi_ack"]) {
        const http = { ...headers(), "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}` };
        // Dulwich sends duplicate wants for HEAD and its target branch.
        const wants = Buffer.concat([packet(`want ${id} ${modes}\n`), packet(`want ${id}\n`), Buffer.from("0000")]);
        const response = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: http, body: Buffer.concat([wants, packet("done\n")]) });
        assert.equal(response.status, 200, modes);
        const bytes = Buffer.from(await response.arrayBuffer());
        assert.deepEqual(bytes.subarray(0, 8), packet("NAK\n"));
        const checked = spawnSync("git", ["--git-dir", repository, "index-pack", "--stdin", "--strict"], { input: bytes.subarray(8), encoding: "utf8", env: gitEnv });
        assert.equal(checked.status, 0, checked.stderr);
        const round = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: http, body: Buffer.concat([wants, packet(`have ${id}\n`), Buffer.from("0000")]) });
        assert.equal(round.status, 200, modes);
        assert.deepEqual(Buffer.from(await round.arrayBuffer()), Buffer.concat([packet(`ACK ${id} common\n`), packet("NAK\n")]));
      }
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
