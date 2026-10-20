import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const execute = promisify(execFile);
test("scale harness preserves reachable warnings and excludes unrelated source objects", { timeout: 120000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-benchmark-contract-"));
  const source = join(root, "source.git"), cwd = fileURLToPath(new URL("../", import.meta.url));
  await mkdir(source);
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null" };
  const git = async (args, input = null) => {
    const child = spawn("git", ["-c", "user.name=Benchmark", "-c", "user.email=benchmark@example.test", ...args], { cwd: source, env, stdio: ["pipe", "pipe", "pipe"] });
    let out = "", err = "";
    child.stdout.on("data", (bytes) => { out += bytes; }); child.stderr.on("data", (bytes) => { err += bytes; });
    const done = new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
    child.stdin.end(input);
    assert.equal(await done, 0, err); return out.trim();
  };
  try {
    await git(["init", "--bare", "--initial-branch=master", "--object-format=sha1"]);
    const blob = await git(["hash-object", "-w", "--stdin"], "historical tree bytes\n");
    const tree = await git(["hash-object", "-w", "--literally", "-t", "tree", "--stdin"], Buffer.concat([Buffer.from("100664 file\0"), Buffer.from(blob, "hex")]));
    const commit = await git(["commit-tree", tree], "reachable tree warning\n");
    await git(["update-ref", "refs/heads/master", commit]);
    await git(["hash-object", "-w", "--literally", "-t", "tag", "--stdin"], `object ${commit}\ntype commit\ntag orphan\n\nunreachable tag warning\n`);
    let previous;
    for (const label of ["fresh", "cached"]) {
      const work = join(root, label), args = ["scripts/benchmark.mjs", "--source", source, "--work-dir", work];
      if (previous) args.push("--baseline-report", previous);
      await execute(process.execPath, args, { cwd, env, timeout: 60000, killSignal: "SIGTERM", maxBuffer: 65536 });
      const report = JSON.parse(await readFile(join(work, "report.json"), "utf8"));
      assert.equal(report.status, "complete");
      const diagnostics = report.phases.find((phase) => phase.phase === "source-visible-diagnostics");
      assert.equal(diagnostics.unrelatedDiagnostics, 1);
      assert.equal(diagnostics.reachableDiagnostics.length, 1);
      assert.ok(diagnostics.reachableDiagnostics[0].includes(`${tree}: badFilemode:`));
      assert.equal(report.phases.at(-1).matchesSource, true);
      if (previous) assert.equal(report.phases[0].reusedFrom, previous);
      previous = join(work, "report.json");
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});
