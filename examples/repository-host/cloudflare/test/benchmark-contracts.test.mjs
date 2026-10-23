import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { FIELDS, checkReadOnly, usageDelta } from "../scripts/benchmark-usage.mjs";
import { schedule } from "../scripts/benchmark-workload.mjs";

const execute = promisify(execFile);
const script = fileURLToPath(new URL("../scripts/hosting-benchmark.mjs", import.meta.url));

function snapshot(epoch = "instance") {
  return { epoch, usage: Object.fromEntries(FIELDS.map((field) => [field, 0])),
    observerUsage: { sqlCalls: 0, sqlFailures: 0, sqlRowsRead: 0, sqlRowsWritten: 0 },
    objects: 1, refs: 1, stagedObjects: 1, stagedChunks: 1, temporaryObjects: 0, temporaryChunks: 0 };
}

test("observer work is excluded while failures and unknown rows remain visible", () => {
  const before = snapshot(), after = snapshot();
  Object.assign(after.usage, { sqlCalls: 12, sqlRowsRead: 24, sqlRowsWritten: 5, sqlFailures: 1, r2Gets: 3, r2Retries: 2, r2Failures: 2 });
  Object.assign(after.observerUsage, { sqlCalls: 10, sqlRowsRead: 20 });
  const delta = usageDelta(before, after);
  assert.deepEqual([delta.sqlCalls, delta.sqlRowsRead, delta.sqlRowsWritten], [2, 4, 5]);
  assert.deepEqual([delta.sqlFailures, delta.r2Gets, delta.r2Failures], [1, 3, 2]);
  checkReadOnly(before, after, delta);
  after.usage.sqlRowsRead = null; after.observerUsage.sqlRowsWritten = null;
  assert.equal(usageDelta(before, after).sqlRowsRead, null);
  assert.equal(usageDelta(before, after).sqlRowsWritten, null);
});

test("restarts, reset counters, missing fields and invalid measurements fail", () => {
  const restarted = snapshot("restart");
  restarted.usage = Object.fromEntries(FIELDS.map((field) => [field, 100]));
  assert.throws(() => usageDelta(snapshot(), restarted), /instance changed/);
  for (const value of [-1, 2 ** 53, true, 1.5, NaN, null, undefined]) {
    const after = snapshot(); after.usage.r2Gets = value;
    assert.throws(() => usageDelta(snapshot(), after), /invalid service counter/);
  }
  const before = snapshot(); before.usage.r2Gets = 3;
  assert.throws(() => usageDelta(before, snapshot()), /reset/);
  const missing = snapshot(); delete missing.observerUsage.sqlCalls;
  assert.throws(() => usageDelta(snapshot(), missing), /missing service counter/);
});

test("observer overcount and fetch storage mutations cannot produce a successful result", () => {
  const after = snapshot(); after.observerUsage.sqlCalls = 1;
  assert.throws(() => usageDelta(snapshot(), after), /observer exceeds/);
  after.observerUsage.sqlCalls = 0; after.objects++;
  assert.throws(() => checkReadOnly(snapshot(), after, usageDelta(snapshot(), after)), /storage/);
  after.objects--; after.usage.r2Puts++;
  assert.throws(() => checkReadOnly(snapshot(), after, usageDelta(snapshot(), after)), /R2/);
});

test("the deterministic workload publishes every historical want before fetching it", () => {
  const plan = schedule();
  assert.equal(plan.filter((item) => item.kind === "push").length, 32);
  assert.equal(plan.filter((item) => item.kind === "fetch").length, 56);
  assert.equal(plan.filter((item) => item.group === "large_push").length, 8);
  let published = 0;
  for (const item of plan) {
    if (item.kind === "push") published++;
    assert.equal(item.generation, published);
    assert.ok(item.want <= published);
    if (item.have !== null) assert.ok(item.have < item.want);
  }
  assert.deepEqual(plan, schedule());
  assert.equal(schedule(4).length, 11);
  for (const value of [0, 3, 5, 36, true, 4.5]) assert.throws(() => schedule(value), /generations/);
});

test("full-stage validation rejects screens before creating evidence", async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-mixed-options-"));
  try {
    for (const args of [["--samples", "2"], ["--samples", "5", "--generations", "4"]]) {
      const work = join(root, "not-created");
      await assert.rejects(execute(process.execPath, [script, "--work-dir", work, "--stage", "full", ...args]), /full validation requires/);
      await assert.rejects(stat(work), { code: "ENOENT" });
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("startup failures preserve an explicitly failed report without a summary", async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-mixed-failed-")), work = join(root, "run");
  try {
    await assert.rejects(execute(process.execPath, [script, "--work-dir", work, "--git", join(root, "missing-git")]), /missing-git/);
    const report = JSON.parse(await readFile(join(work, "report.json"), "utf8"));
    assert.equal(report.status, "failed");
    assert.ok(report.failure.includes("missing-git"));
    assert.deepEqual(report.trials, []);
    assert.equal(report.summary, undefined);
  } finally { await rm(root, { recursive: true, force: true }); }
});
