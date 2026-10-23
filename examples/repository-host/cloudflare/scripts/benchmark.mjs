// Ordinary Git is the reference client only. The server is the own Wasm host
// with actual local Wrangler R2/SQLite, or direct workerd with the S3 fixture.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, readFile, readdir, rename, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { headers, TOKEN } from "../test/wrangler.mjs";
import { freezeRuntime, startRuntime } from "./benchmark-runtime.mjs";

const args = process.argv.slice(2);
const options = Object.fromEntries(args.reduce((pairs, value, index) => { if (index % 2 === 0) pairs.push([value, args[index + 1]]); return pairs; }, []));
if (args.length % 2 || !options["--source"] || !options["--work-dir"]) throw new Error("usage: node scripts/benchmark.mjs --source REPO --work-dir NEW_DIRECTORY [--revision HEAD] [--runtime wrangler|workerd] [--baseline-report MATCHING_SOURCE_REPORT]");
const root = fileURLToPath(new URL("../", import.meta.url));
const source = resolve(options["--source"]), work = resolve(options["--work-dir"]);
const bundle = join(work, "frozen");
const runtime = options["--runtime"] ?? "wrangler";
if (!["wrangler", "workerd"].includes(runtime)) throw new Error("unknown runtime");
await mkdir(work); // Existing evidence is never overwritten.
const report = { status: "running", runtime, source, revision: null, codecCommit: null, phases: [], resources: { processTreePeakRssKiB: 0, observedProcessTreeCpuSeconds: 0, scope: "local server process tree; includes development/runtime/storage infrastructure, not Cloudflare Worker billing CPU" } };
let saving = Promise.resolve();
const save = () => {
  const snapshot = `${JSON.stringify(report, null, 2)}\n`;
  saving = saving.then(async () => { await writeFile(join(work, "report.pending"), snapshot); await rename(join(work, "report.pending"), join(work, "report.json")); });
  return saving;
};
const progress = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
let server, phase = "prepare", recent = "", sampling = false;
let interrupted = false;
const commands = new Set();
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => { interrupted = true; for (const child of commands) child.kill(signal); });
const started = performance.now(), observed = new Map();

async function command(binary, argv, cwd, allowFailure = false, onStdout = null) {
  const child = spawn(binary, argv, { cwd, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0", WRANGLER_SEND_METRICS: "false" }, stdio: ["ignore", "pipe", "pipe"] });
  commands.add(child);
  let stdout = "", stderr = "";
  child.stdout.on("data", (bytes) => { onStdout?.(bytes); stdout = (stdout + bytes).slice(-65536); });
  child.stderr.on("data", (bytes) => { stderr = (stderr + bytes).slice(-65536); recent = stderr.split(/[\r\n]/).filter(Boolean).at(-1) ?? ""; });
  const code = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
  commands.delete(child);
  if (interrupted) throw new Error("benchmark interrupted");
  if (code !== 0 && !allowFailure) throw new Error(`${binary} ${argv[0]} failed (${code})\n${stderr}`);
  return { code, stdout: stdout.trim(), stderr: stderr.trim() };
}
const git = (directory, argv, allowFailure = false, onStdout = null) => command("git", ["-c", "user.name=Git zero benchmark", "-c", "user.email=benchmark@example.test", "-c", "pack.threads=1", "-c", "index.threads=1", "-c", "pack.windowMemory=64m", "-c", "http.extraHeader=Authorization: Bearer " + TOKEN, ...argv], directory, allowFailure, onStdout);

async function phaseRun(name, operation) {
  phase = name; recent = ""; const began = performance.now(); progress({ phase, event: "start" });
  const result = await operation();
  report.phases.push({ phase: name, seconds: (performance.now() - began) / 1000, ...result });
  await save(); progress({ phase, event: "complete", seconds: report.phases.at(-1).seconds });
  return result;
}
async function resources() {
  if (sampling || !server?.runtimePid) return;
  sampling = true;
  try {
    const pending = [server.runtimePid], seen = new Set(); let rss = 0;
    while (pending.length) {
      const pid = pending.pop(); if (seen.has(pid)) continue; seen.add(pid);
      try {
        const [status, stat, children] = await Promise.all([
          readFile(`/proc/${pid}/status`, "utf8"), readFile(`/proc/${pid}/stat`, "utf8"), readFile(`/proc/${pid}/task/${pid}/children`, "utf8"),
        ]);
        const fields = stat.slice(stat.lastIndexOf(")") + 2).trim().split(/\s+/);
        const ticks = Number(fields[11]) + Number(fields[12]), identity = `${pid}:${fields[19]}`;
        observed.set(identity, ticks); rss += Number(/^VmRSS:\s+(\d+)/m.exec(status)?.[1] ?? 0);
        pending.push(...children.trim().split(/\s+/).filter(Boolean).map(Number));
      } catch (error) { if (!["ENOENT", "ESRCH", "EACCES"].includes(error.code)) throw error; }
    }
    report.resources.processTreePeakRssKiB = Math.max(report.resources.processTreePeakRssKiB, rss);
    report.resources.observedProcessTreeCpuSeconds = [...observed.values()].reduce((a, b) => a + b, 0) / report.resources.clockTicksPerSecond;
  } finally { sampling = false; }
}
let sampleTimer, progressTimer;
async function durableFootprint(directory) {
  const groups = Object.fromEntries(["repositoryMetadata", "objectStorage", "storageFixtureMetadata", "other"].map((name) => [name, { files: 0, bytes: 0, allocatedBytes: 0 }]));
  async function visit(relative = "") {
    for (const entry of await readdir(join(directory, relative), { withFileTypes: true })) {
      const path = join(relative, entry.name);
      if (entry.isDirectory()) { await visit(path); continue; }
      if (!entry.isFile()) continue;
      const group = runtime === "workerd"
        ? (path.startsWith("r2/") ? "objectStorage" : path.startsWith("sqlite/") ? "repositoryMetadata" : "other")
        : path.includes("/r2/") ? (path.includes("/blobs/") ? "objectStorage" : "storageFixtureMetadata") : path.includes("/do/") ? "repositoryMetadata" : "other";
      const metadata = await stat(join(directory, path));
      groups[group].files++; groups[group].bytes += metadata.size; groups[group].allocatedBytes += metadata.blocks * 512;
    }
  }
  await visit();
  return { groups, scope: "Local durable state only, excluding the source, reference clone and runtime bundle. objectStorage is remote R2 in production; storageFixtureMetadata belongs to the local R2 mock. bytes are logical file sizes; allocatedBytes are local filesystem blocks, including WAL files." };
}
try {
  report.codecCommit = (await git(fileURLToPath(new URL("../../../../", import.meta.url)), ["rev-parse", "HEAD"])).stdout;
  report.workingTreeClean = (await git(fileURLToPath(new URL("../../../../", import.meta.url)), ["status", "--porcelain"])).stdout.length === 0;
  report.revision = (await git(source, ["rev-parse", options["--revision"] ?? "HEAD"])).stdout;
  report.objectFormat = (await git(source, ["rev-parse", "--show-object-format"])).stdout;
  assert.equal((await git(source, ["rev-parse", "--is-shallow-repository"])).stdout, "false", "the scale proof requires a complete source repository");
  report.resources.clockTicksPerSecond = Number((await command("getconf", ["CLK_TCK"], root)).stdout);
  report.resources.sampleIntervalMs = 1000;
  progressTimer = setInterval(() => { progress({ phase, elapsedSeconds: (performance.now() - started) / 1000, recentClientProgress: recent, resources: report.resources }); save().catch(() => {}); }, 20000);
  await phaseRun("freeze-runtime", async () => {
    await freezeRuntime(bundle, runtime, report.objectFormat === "sha256" ? 1 : 0,
      { run: (binary, args) => command(binary, args, root) });
    return { directory: bundle };
  });
  const baseline = await phaseRun("source-strict-fsck", async () => {
    if (options["--baseline-report"]) {
      const previous = JSON.parse(await readFile(options["--baseline-report"], "utf8"));
      assert.equal(previous.source, source); assert.equal(previous.revision, report.revision); assert.equal(previous.objectFormat, report.objectFormat);
      const checked = previous.phases.find((item) => item.phase === "source-strict-fsck");
      assert.equal(checked?.exitCode, 0, "only a successful source integrity check may be reused");
      return { exitCode: checked.exitCode, diagnostics: checked.diagnostics, reusedFrom: resolve(options["--baseline-report"]), originalSeconds: checked.seconds };
    }
    const result = await git(source, ["fsck", "--full", "--strict"], true);
    const diagnostics = result.stderr.split("\n").filter((line) => /^(warning|error) in /.test(line)).sort();
    assert.ok(result.code === 0 || (diagnostics.length > 0 && diagnostics.every((line) => line.includes(": badFilemode:"))), `source integrity check failed\n${result.stderr}`);
    return { exitCode: result.code, diagnostics };
  });
  await phaseRun("source-visible-diagnostics", async () => {
    // fsck checks unreachable objects too. An ordinary push sends the selected
    // commit closure; unrelated legacy tags/dangling objects are not cloned.
    // Keep only a tiny set of diagnostic IDs while streaming the reference walk.
    const ids = new Set(baseline.diagnostics.map((line) => / in \w+ ([0-9a-f]+):/.exec(line)[1]));
    const reachable = new Set(); let pending = "";
    if (ids.size) await git(source, ["rev-list", "--objects", "--no-object-names", report.revision], false, (bytes) => {
      const lines = (pending + bytes).split("\n"); pending = lines.pop();
      for (const id of lines) if (ids.has(id)) reachable.add(id);
    });
    baseline.allSourceDiagnostics = baseline.diagnostics;
    baseline.diagnostics = baseline.diagnostics.filter((line) => reachable.has(/ in \w+ ([0-9a-f]+):/.exec(line)[1]));
    return { reachableDiagnostics: baseline.diagnostics, unrelatedDiagnostics: baseline.allSourceDiagnostics.length - baseline.diagnostics.length };
  });
  const persist = join(work, "state");
  server = await startRuntime(persist, bundle, runtime, report.objectFormat === "sha256" ? 1 : 0);
  report.origin = server.origin; await save();
  const repo = `${server.origin}/kernel.git`;
  sampleTimer = setInterval(() => { resources().catch((error) => { report.resourceError = error.message; }); }, 1000);
  await phaseRun("push", async () => {
    await git(source, ["-c", "protocol.version=2", "push", "--progress", "--atomic", repo, `${report.revision}:refs/heads/master`]);
    const response = await fetch(`${server.origin}/_test/kernel/state`, { headers: headers() }); assert.equal(response.status, 200);
    return { state: await response.json(), durableFootprint: await durableFootprint(persist) };
  });
  const clone = join(work, "clone.git");
  await phaseRun("clone", async () => {
    await git(work, ["-c", "protocol.version=2", "clone", "--mirror", "--progress", repo, clone]);
    assert.equal((await git(clone, ["rev-parse", "HEAD"])).stdout, report.revision);
    return {};
  });
  await phaseRun("incremental-push", async () => {
    const tree = (await git(source, ["rev-parse", `${report.revision}^{tree}`])).stdout;
    report.incrementalRevision = (await git(source, ["commit-tree", tree, "-p", report.revision, "-m", "git-zero incremental hosting verification"])).stdout;
    // The reference fixture's branch is unchanged; only a new unreachable
    // test commit is written by the ordinary Git client.
    await git(source, ["push", "--progress", "--atomic", repo, `${report.incrementalRevision}:refs/heads/master`]);
    return { revision: report.incrementalRevision };
  });
  await phaseRun("cold-fetch", async () => {
    await server.stop();
    server = await startRuntime(persist, bundle, runtime, report.objectFormat === "sha256" ? 1 : 0);
    // Keep the ordinary client remote, then fetch from the restarted endpoint.
    await git(clone, ["remote", "set-url", "origin", `${server.origin}/kernel.git`]);
    await git(clone, ["fetch", "--progress", "origin"]);
    assert.equal((await git(clone, ["rev-parse", "HEAD"])).stdout, report.incrementalRevision);
    const response = await fetch(`${server.origin}/_test/kernel/state`, { headers: headers() }); assert.equal(response.status, 200);
    return { state: await response.json(), durableFootprint: await durableFootprint(persist) };
  });
  await phaseRun("clone-strict-fsck", async () => {
    const result = await git(clone, ["fsck", "--full", "--strict"], true);
    const diagnostics = result.stderr.split("\n").filter((line) => /^(warning|error) in /.test(line)).sort();
    assert.equal(result.code, baseline.exitCode); assert.deepEqual(diagnostics, baseline.diagnostics);
    return { exitCode: result.code, diagnostics, matchesSource: true };
  });
  report.status = "complete";
} catch (error) {
  report.status = "failed"; report.failure = { phase, message: error.message, runtimeLogs: server?.logs() ?? "" };
  process.exitCode = 1;
} finally {
  clearInterval(sampleTimer); clearInterval(progressTimer); await resources();
  await server?.stop(); report.seconds = (performance.now() - started) / 1000; await save();
  progress({ status: report.status, report: join(work, "report.json"), failure: report.failure?.message });
}
