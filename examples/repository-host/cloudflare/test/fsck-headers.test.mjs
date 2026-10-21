import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { entry, object, pack, project, receiveReport, wire } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

const git = promisify(execFile);

function size(value) {
  const bytes = [];
  do { const byte = value & 127; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value);
  return Buffer.from(bytes);
}

function literalDelta(base, result) {
  const bytes = [size(base.length), size(result.length)];
  for (let at = 0; at < result.length; at += 127) {
    const part = result.subarray(at, at + 127); bytes.push(Buffer.from([part.length]), part);
  }
  return Buffer.concat(bytes);
}

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} enforces strict receive identities for plain and thin-delta objects`, { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-fsck-headers-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      const namespace = `headers-${format}-${randomUUID()}`, fixture = project(format);
      const remote = `${server.origin}/${namespace}.git/git-receive-pack`;
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const send = async (value, packed, name) => {
        const response = await fetch(remote, { method: "POST", headers: headers(), body: wire(format, packed, value.id, name) });
        assert.equal(response.status, 200); return receiveReport(await response.arrayBuffer());
      };
      assert.match(await send(fixture.root, fixture.pack, "refs/heads/main"), /unpack ok/);
      for (const [field, identity] of [["author", "x"], ["committer", "A <a@b> 01 +0000"], ["author", "A <a@b> 9223372036854775808 +0000"]]) {
        const body = fixture.root.body.toString().replace(new RegExp(`${field} [^\\n]+`), `${field} ${identity}`);
        const value = object(format, 1, body);
        for (const encoded of [entry(1, value.body), entry(7, literalDelta(fixture.root.body, value.body), fixture.root.id)]) {
          assert.match(await send(value, pack(format, [encoded]), "refs/heads/rejected"), /unpack invalid/);
          const response = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() });
          const state = await response.json(); assert.equal(state.refs, 1); assert.equal(state.objects, fixture.objects.length);
          assert.equal(state.stages.length, 1); assert.equal(state.temporaryObjects, 0);
        }
      }
      for (const [tagger, accepted] of [["tagger x\n", false], ["", true], ["tagger  <a@b> \t1 +9999\n", true]]) {
        const value = object(format, 4, `object ${fixture.root.id.toString("hex")}\ntype commit\ntag old\n${tagger}\nmessage\n`);
        const report = await send(value, pack(format, [entry(4, value.body)]), `refs/tags/case-${randomUUID()}`);
        assert.match(report, accepted ? /unpack ok/ : /unpack invalid/);
      }
      const commit = object(format, 1, `tree ${fixture.objects[1].id.toString("hex")}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n`);
      assert.match(await send(commit, pack(format, [entry(1, commit.body)]), "refs/heads/newline"), /unpack ok/);
      const tag = object(format, 4, `object ${commit.id.toString("hex")}\ntype commit\ntag old\n`);
      assert.match(await send(tag, pack(format, [entry(4, tag.body)]), "refs/tags/newline"), /unpack ok/);
      for (const value of [commit, tag]) {
        const truncated = object(format, value.kind, value.body.subarray(0, -1));
        assert.match(await send(truncated, pack(format, [entry(value.kind, truncated.body)]), "refs/tags/truncated"), /unpack invalid/);
      }
      for (const tail of ["tagger A <a@b> 0 +0000\n\nmessage\n", "\nmessage\n", ""]) {
        const legacy = object(format, 4, `object ${commit.id.toString("hex")}\ntype commit\ntag \n${tail}`);
        assert.match(await send(legacy, pack(format, [entry(4, legacy.body)]), `refs/tags/legacy-${randomUUID()}`), /unpack ok/);
      }
      for (const tail of ["tag \ntagger x\n\nmessage\n", "\nmessage\n"]) {
        const invalid = object(format, 4, `object ${commit.id.toString("hex")}\ntype commit\n${tail}`);
        assert.match(await send(invalid, pack(format, [entry(4, invalid.body)]), `refs/tags/invalid-${randomUUID()}`), /unpack invalid/);
      }
      const clone = join(directory, `clone-${format}.git`);
      const options = { timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } };
      await git("git", ["-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, "clone", "--mirror", `${server.origin}/${namespace}.git`, clone], options);
      await git("git", ["-C", clone, "fsck", "--full", "--strict"], options);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
