import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { Store } from "../src/storage.mjs";
import { entry, object, pack, receiveReport, wire } from "./fixture.mjs";
import { startStandalone } from "./standalone.mjs";
import { headers, TOKEN } from "./wrangler.mjs";

const execute = promisify(execFile), SIZE = 9 * 1024 * 1024;
const env = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" };
function variable(value) {
  const bytes = [];
  do { const byte = value & 127; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value);
  return Buffer.from(bytes);
}
function fixture(format) {
  const base = object(format, 3, Buffer.alloc(SIZE, 97));
  const target = object(format, 3, Buffer.concat([base.body, Buffer.from("!")]));
  const delta = Buffer.concat([variable(SIZE), variable(SIZE + 1), Buffer.from([0xf0, SIZE & 255, (SIZE >>> 8) & 255, (SIZE >>> 16) & 255, 1, 33])]);
  return { base, target, delta };
}
async function state(server) {
  const response = await fetch(`${server.origin}/_test/cleanup/state`, { headers: headers() });
  assert.equal(response.status, 200); return response.json();
}
async function push(server, format, entries, target, name) {
  const response = await fetch(`${server.origin}/cleanup.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, pack(format, entries), target.id, name) });
  assert.equal(response.status, 200, await response.clone().text());
  const report = receiveReport(await response.arrayBuffer());
  assert.ok(report.includes(`ok ${name}`), report);
}

test("temporary cleanup defers only after recovery is scheduled", async () => {
  const deletion = new Error("delete unavailable"), alarmFailure = new Error("alarm unavailable");
  let alarms = 0;
  const storage = Object.create(Store.prototype);
  storage.exec = () => [{ object: "body", seq: 0, key: "chunk" }];
  storage.delete = async () => { throw deletion; };
  storage.ctx = { storage: { async setAlarm() { alarms++; } } };
  assert.equal(await storage.clearTemporary("stage", { defer: true }), false);
  assert.equal(alarms, 1);
  await assert.rejects(storage.clearTemporary("stage"), (error) => error === deletion);
  assert.equal(alarms, 2);
  storage.ctx.storage.setAlarm = async () => { throw alarmFailure; };
  await assert.rejects(storage.clearTemporary("stage", { defer: true }), (error) => error === alarmFailure);
});

test("verified receives survive temporary deletion failures and retain recovery claims", { timeout: 60000 }, async () => {
  for (const format of [0, 1]) {
    const directory = await mkdtemp(join(tmpdir(), "git-zero-intake-temp-cleanup-"));
    let server;
    try {
      server = await startStandalone(directory, { format, deleteFailures: 3 });
      const { base, target, delta } = fixture(format);
      await push(server, format, [entry(3, base.body), entry(7, delta, base.id)], target, "refs/tags/large");
      assert.equal(server.storageFaults.deleteFailures, 3);
      const pending = await state(server);
      assert.equal(pending.refs, 1); assert.equal(pending.objects, 2);
      assert.equal(pending.stages.filter((stage) => stage.phase === "published").length, 1);
      assert.ok(pending.temporaryChunks > 0); assert.ok(pending.temporaryObjects > 0);
      await server.stop(); server = await startStandalone(directory, { format });
      const recovered = await state(server);
      assert.equal(recovered.refs, 1); assert.equal(recovered.objects, 2);
      assert.equal(recovered.temporaryChunks, 0); assert.equal(recovered.temporaryObjects, 0);
      assert.equal(recovered.stages.length, 1); assert.equal(recovered.stages[0].phase, "published");
    } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
  }
});

test("native fetch gets a clean EOF when temporary deletion is deferred", { timeout: 60000 }, async () => {
  for (const format of [0, 1]) {
    const directory = await mkdtemp(join(tmpdir(), "git-zero-fetch-temp-cleanup-"));
    let server;
    try {
      const persist = join(directory, "state"), client = join(directory, "client.git");
      server = await startStandalone(persist, { format });
      const { base, target, delta } = fixture(format);
      await push(server, format, [entry(3, base.body)], base, "refs/tags/base");
      await push(server, format, [entry(7, delta, base.id)], target, "refs/tags/large");
      await server.stop(); server = await startStandalone(persist, { format, deleteFailures: 3 });
      await execute("git", ["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, client], { env });
      const result = await execute("git", ["-C", client, "-c", "protocol.version=0", "-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, "fetch", "--quiet", "--no-tags", `${server.origin}/cleanup.git`, "refs/tags/large:refs/tags/large"], { env, timeout: 30000 });
      assert.ok(!result.stderr.includes("RPC failed"));
      assert.equal((await execute("git", ["-C", client, "rev-parse", "refs/tags/large"], { env })).stdout.trim(), target.id.toString("hex"));
      assert.equal((await execute("git", ["-C", client, "cat-file", "-s", target.id.toString("hex")], { env })).stdout.trim(), String(SIZE + 1));
      await execute("git", ["-C", client, "fsck", "--full", "--strict"], { env });
      assert.equal(server.storageFaults.deleteFailures, 3);
      const pending = await state(server);
      assert.equal(pending.refs, 2); assert.equal(pending.objects, 2);
      assert.ok(pending.temporaryChunks > 0); assert.ok(pending.temporaryObjects > 0);
      await server.stop(); server = await startStandalone(persist, { format });
      const recovered = await state(server);
      assert.equal(recovered.refs, 2); assert.equal(recovered.objects, 2);
      assert.equal(recovered.temporaryChunks, 0); assert.equal(recovered.temporaryObjects, 0);
    } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
  }
});

test("invalid receives remain rejected when temporary cleanup is deferred", { timeout: 60000 }, async () => {
  for (const format of [0, 1]) {
    const directory = await mkdtemp(join(tmpdir(), "git-zero-invalid-temp-cleanup-"));
    let server;
    try {
      server = await startStandalone(directory, { format, deleteFailures: 6 });
      const { base, target } = fixture(format);
      // A valid size header warms the large base, then opcode zero fails.
      const invalid = Buffer.concat([variable(SIZE), variable(SIZE + 1), Buffer.from([0])]);
      const response = await fetch(`${server.origin}/cleanup.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, pack(format, [entry(3, base.body), entry(7, invalid, base.id)]), target.id) });
      const report = await response.arrayBuffer();
      assert.ok(response.status === 400 || (response.status === 200 && receiveReport(report).includes("unpack invalid")));
      assert.equal(server.storageFaults.deleteFailures, 6);
      const pending = await state(server);
      assert.equal(pending.refs, 0); assert.equal(pending.objects, 0);
      assert.ok(pending.temporaryChunks > 0);
      assert.ok(pending.stages.every((stage) => stage.phase === "failed"));
      await server.stop(); server = await startStandalone(directory, { format });
      const recovered = await state(server);
      assert.equal(recovered.refs, 0); assert.equal(recovered.objects, 0);
      assert.equal(recovered.temporaryChunks, 0); assert.equal(recovered.temporaryObjects, 0);
      assert.equal(recovered.stagedChunks, 0); assert.deepEqual(recovered.stages, []);
    } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
  }
});
