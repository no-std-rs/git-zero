import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { entry, object, pack, project, wire } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

const git = promisify(execFile);
async function state(server, namespace) {
  const response = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() });
  assert.equal(response.status, 200); return response.json();
}

async function report(response) {
  assert.equal(response.status, 200);
  const bytes = Buffer.from(await response.arrayBuffer()), pieces = [];
  let cursor = 0;
  while (cursor < bytes.length) {
    const length = Number.parseInt(bytes.subarray(cursor, cursor + 4).toString(), 16);
    if (length === 0) { assert.equal(cursor + 4, bytes.length); break; }
    assert.ok(length >= 5 && cursor + length <= bytes.length);
    assert.equal(bytes[cursor + 4], 1);
    pieces.push(bytes.subarray(cursor + 5, cursor + length)); cursor += length;
  }
  return Buffer.concat(pieces).toString();
}

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} scans plain structural objects before discarding decoded bytes`, { timeout: 120000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-plain-intake-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      const fixture = project(format);
      // Exceed both the one MiB representation seed and eight MiB canonical
      // capture limits. Link scanning can still consume the original stream.
      const commit = object(format, 1, Buffer.concat([fixture.root.body, Buffer.alloc(9 * 1024 * 1024, 122)]));
      const objects = [...fixture.objects.slice(0, -1), commit];
      const packed = pack(format, objects.map((value) => entry(value.kind, value.body)));
      const namespace = `plain-${format}-${randomUUID()}`;
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, packed, commit.id, "refs/heads/main") });
      assert.match(await report(response), /unpack ok/);
      const published = await state(server, namespace);
      assert.equal(published.refs, 1); assert.equal(published.objects, objects.length);
      assert.equal(published.treeNameScratchRows, 0);
      t.diagnostic(`${runtime} format=${format} intake R2 gets=${published.usage.r2Gets} puts=${published.usage.r2Puts}`);
      assert.equal(published.usage.r2Gets, 0, "plain intake must not reread the stored pack to scan links");
      assert.equal(published.usage.r2Puts, 1, "plain intake must not spool a canonical body that is not needed yet");

      const clone = join(directory, `clone-${format}.git`);
      const options = { timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } };
      await git("git", ["-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, "clone", "--mirror", `${server.origin}/${namespace}.git`, clone], options);
      await git("git", ["-C", clone, "fsck", "--full", "--strict"], options);
      assert.equal((await git("git", ["-C", clone, "rev-parse", "HEAD"], options)).stdout.trim(), commit.id.toString("hex"));

      const rejected = `plain-corrupt-${format}-${randomUUID()}`;
      const initBad = await fetch(`${server.origin}/_test/${rejected}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initBad.status, 200); await initBad.arrayBuffer();
      const damaged = Buffer.from(packed); damaged[damaged.length - 1] ^= 1;
      const invalid = await fetch(`${server.origin}/${rejected}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, damaged, commit.id, "refs/heads/main") });
      assert.match(await report(invalid), /unpack invalid/);
      const clean = await state(server, rejected);
      assert.equal(clean.refs, 0); assert.equal(clean.objects, 0);
      assert.equal(clean.treeNameScratchRows, 0); assert.deepEqual(clean.stages, []);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
