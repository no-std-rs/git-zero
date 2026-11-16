// Run on an otherwise idle test machine. MAIN must be an archive of main under
// this checkout, built with its own codec. Generated evidence is never committed.
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadavg, availableParallelism } from "node:os";
const head = fileURLToPath(new URL("../", import.meta.url)), root = resolve(head, "../../.."), main = resolve(process.argv[2] ?? "");
if (!main.startsWith(root + "/.scratch/")) throw new Error("supply a generated main archive under this checkout's ignored .scratch/");
const output = join(root, ".scratch/review4-tree-timings"); await mkdir(output, { recursive: true });
const samples = [], median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
for (let trial = 0; trial < 5; trial++) for (const variant of trial % 2 ? ["head", "main"] : ["main", "head"]) {
  const cwd = variant === "head" ? head : main, began = performance.now(), loadBefore = loadavg();
  const child = spawn(process.execPath, ["--test", "--test-concurrency=1", "--test-reporter=spec", "test/tree-depths.test.mjs"], { cwd, env: { ...process.env, NO_COLOR: "1" }, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "", stderr = ""; child.stdout.on("data", (data) => { stdout += data; }); child.stderr.on("data", (data) => { stderr += data; });
  const code = await new Promise((r) => child.once("exit", r));
  await writeFile(join(output, `${variant}-${trial}.log`), stdout + stderr);
  const tests = [...stdout.matchAll(/^✔ (.+) \(([0-9.]+)ms\)$/gm)].map((m) => ({ name: m[1], ms: Number(m[2]) }));
  samples.push({ variant, trial, code, tests, wallMs: performance.now() - began, loadBefore, loadAfter: loadavg(), cpus: availableParallelism() });
  await writeFile(join(output, "samples.json"), JSON.stringify(samples, null, 2)); console.log(JSON.stringify(samples.at(-1)));
  if (code || tests.length !== 2) throw new Error(`tree-depth run failed; see ${output}`);
}
const medians = {};
for (const variant of ["main", "head"]) for (const name of samples[0].tests.map((t) => t.name)) medians[`${variant}: ${name}`] = median(samples.filter((s) => s.variant === variant).flatMap((s) => s.tests.filter((t) => t.name === name).map((t) => t.ms)));
await writeFile(join(output, "medians.json"), JSON.stringify(medians, null, 2)); console.log(JSON.stringify({ medians }));
