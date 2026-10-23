import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { test } from "node:test";

const execute = promisify(execFile);
const script = fileURLToPath(new URL("../scripts/hosting-benchmark.mjs", import.meta.url));

for (const [runtime, format, cache, protocol] of [
  ["workerd", "sha1", "warm", 0], ["workerd", "sha256", "cold", 2],
  ["wrangler", "sha1", "cold", 1], ["wrangler", "sha256", "warm", 2],
]) test(runtime + " " + format + " " + cache + " mixed workload verifies history and accounting", { timeout: 180000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-mixed-contract-")), work = join(root, "run");
  try {
    await execute(process.execPath, [script, "--work-dir", work, "--runtime", runtime,
      "--object-format", format, "--view-cache", cache, "--protocol", String(protocol),
      "--generations", "4", "--blob-bytes", "4096", "--samples", "1"],
    { timeout: 150000, maxBuffer: 65536 });
    const report = JSON.parse(await readFile(join(work, "report.json"), "utf8"));
    assert.equal(report.status, "complete");
    assert.equal(report.identity.stage, "screen");
    assert.equal(report.identity.generations, 4);
    assert.equal(report.identity.referenceMaintenance.length, 2);
    assert.ok(report.identity.bundle.some((file) => file.path.endsWith(".wasm")));
    assert.ok(report.identity.bundle.every((file) => file.bytes > 0 && /^[0-9a-f]{64}$/.test(file.sha256)));
    assert.ok(report.unknownBillingDimensions.includes("Worker CPU"));
    const trial = report.trials[0], epochs = new Set();
    assert.equal(trial.operations.length, 11);
    for (const row of trial.operations) {
      assert.deepEqual(Object.keys(row.backends).sort(), ["git", "own"]);
      for (const backend of Object.values(row.backends)) assert.ok(backend.readySeconds > 0);
      const own = row.backends.own;
      epochs.add(own.epoch);
      assert.ok(own.usage.sqlCalls > 0);
      assert.equal(own.usage.r2Failures, 0);
      assert.equal(own.usage.sqlFailures, 0);
      if (row.operation.kind === "fetch") {
        assert.equal(own.usage.r2Puts, 0);
        assert.equal(own.usage.r2Deletes, 0);
      } else assert.ok(row.backends.git.maintenanceSeconds > 0);
    }
    assert.equal(epochs.size > 1, cache === "cold");
    assert.equal(report.summary.history_no_haves.observations, 1);
    assert.equal(report.summary.history_with_haves.observations, 1);
    await assert.rejects(execute(process.execPath, [script, "--work-dir", work]), /EEXIST/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("interruption retains partial evidence and closes both local backends", { timeout: 60000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-mixed-interrupted-")), work = join(root, "run");
  const child = spawn(process.execPath, [script, "--work-dir", work, "--runtime", "workerd", "--generations", "4", "--blob-bytes", "4096", "--samples", "1"], { stdio: ["ignore", "pipe", "pipe"] });
  let output = "", errors = "", interrupted = false, forced = false;
  child.stdout.on("data", (bytes) => { output += bytes; if (!interrupted && output.includes('"operation":1')) { interrupted = true; child.kill("SIGTERM"); } });
  child.stderr.on("data", (bytes) => { errors += bytes; });
  const timeout = setTimeout(() => { forced = true; child.kill("SIGKILL"); }, 45000);
  try {
    const [code] = await once(child, "exit");
    assert.equal(forced, false, errors);
    assert.equal(interrupted, true, errors);
    assert.notEqual(code, 0);
    const report = JSON.parse(await readFile(join(work, "report.json"), "utf8"));
    assert.equal(report.status, "failed");
    assert.ok(report.failure);
    assert.ok(report.trials[0].operations.length >= 1 && report.trials[0].operations.length < 11);
    assert.equal(report.summary, undefined);
  } finally { clearTimeout(timeout); child.kill(); await rm(root, { recursive: true, force: true }); }
});
