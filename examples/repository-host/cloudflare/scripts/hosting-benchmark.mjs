// Paired mixed-history measurements of the streamed host and tuned Git HTTP.
import { mkdir, rename, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { TOKEN } from "../test/wrangler.mjs";
import { freezeRuntime, startRuntime } from "./benchmark-runtime.mjs";
import { Git, MAINTENANCE, fixture, reference, seedClient, verify } from "./benchmark-git.mjs";
import { checkReadOnly, measuredUsage, UNKNOWN_BILLING } from "./benchmark-usage.mjs";
import { schedule, SEED } from "./benchmark-workload.mjs";

const repositoryRoot = fileURLToPath(new URL("../../../../", import.meta.url));
const controller = new AbortController();
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => controller.abort());
const authentication = ["-c", "http.extraHeader=Authorization: Bearer " + TOKEN];

function options(args) {
  if (args.length % 2) throw new Error("options require a value");
  const fields = { "--work-dir": "workDir", "--runtime": "runtime", "--object-format": "objectFormat", "--view-cache": "viewCache",
    "--generations": "generations", "--blob-bytes": "blobBytes", "--samples": "samples", "--stage": "stage", "--protocol": "protocol", "--git": "git" };
  const result = { runtime: "wrangler", objectFormat: "sha1", viewCache: "warm", generations: 32, blobBytes: 1024 * 1024, samples: 2, stage: "screen", protocol: 2, git: "git" };
  const seen = new Set();
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    if (!Object.hasOwn(fields, flag) || seen.has(flag)) throw new Error("unknown or duplicate option: " + flag);
    seen.add(flag); result[fields[flag]] = args[index + 1];
  }
  for (const field of ["generations", "blobBytes", "samples", "protocol"]) {
    result[field] = Number(result[field]);
    if (!Number.isSafeInteger(result[field])) throw new Error("option requires a safe integer: " + field);
  }
  schedule(result.generations);
  if (!result.workDir || !["wrangler", "workerd"].includes(result.runtime) || !["sha1", "sha256"].includes(result.objectFormat)
      || !["warm", "cold"].includes(result.viewCache) || !["screen", "full"].includes(result.stage) || ![0, 1, 2].includes(result.protocol)
      || result.samples < 1 || result.samples > 20 || result.blobBytes < 1 || result.blobBytes > 64 * 1024 * 1024) throw new Error("invalid benchmark options; --work-dir is required");
  if (result.stage === "full" && (result.samples < 5 || result.generations !== 32)) throw new Error("full validation requires at least five pairs and all 32 generations");
  return { ...result, workDir: resolve(result.workDir) };
}

async function save(directory, report) {
  const pending = join(directory, "report.pending");
  await writeFile(pending, JSON.stringify(report, null, 2) + "\n");
  await rename(pending, join(directory, "report.json"));
}

async function state(backend) {
  const response = await fetch(backend.server.origin + "/_test/benchmark/state", {
    headers: { authorization: "Bearer " + TOKEN }, signal: AbortSignal.any([controller.signal, AbortSignal.timeout(30000)]),
  });
  if (response.status !== 200) throw new Error("state observation failed: " + response.status);
  return response.json();
}

async function measure(git, directory, args, backend, item, protocol) {
  const began = performance.now();
  await git.run(directory, ["-c", "protocol.version=" + protocol, ...authentication, ...args]);
  const wireSeconds = (performance.now() - began) / 1000;
  let maintenanceSeconds = 0;
  if (backend.name === "git" && item.kind === "push") {
    const start = performance.now();
    for (const command of MAINTENANCE) await git.run(backend.repository, command);
    maintenanceSeconds = (performance.now() - start) / 1000;
  }
  return { wireSeconds, maintenanceSeconds, readySeconds: wireSeconds + maintenanceSeconds };
}

async function verifyPush(git, source, url, want, generation) {
  const refs = new Map((await git.text(source, [...authentication, "ls-remote", url])).split("\n").map((line) => line.split(/\s+/).reverse()));
  for (const name of ["refs/heads/main", "refs/heads/history-" + generation]) if (refs.get(name) !== want) throw new Error("published refs differ from pushed generation");
}

async function operation(context, item, backend) {
  const { directory, source, tips, config, git } = context;
  const own = backend.name === "own", want = tips[item.want - 1], have = item.have ? tips[item.have - 1] : null;
  if (own && item.kind === "fetch" && config.viewCache === "cold") {
    await backend.server.stop();
    backend.server = await startRuntime(backend.persist, context.bundle, config.runtime, Number(config.objectFormat === "sha256"));
  }
  const url = own ? backend.server.origin + "/benchmark.git" : backend.origin;
  let client = source, args;
  if (item.kind === "fetch") {
    client = join(directory, backend.name + "-" + item.generation + "-" + item.group);
    await seedClient(client, source, have, want, git, config.objectFormat);
    args = ["fetch", "--no-tags", url, item.ref + ":refs/heads/result"];
  } else args = ["push", "--atomic", url, want + ":refs/heads/main", want + ":refs/heads/history-" + item.generation];
  const before = own ? await state(backend) : null;
  const result = await measure(git, client, args, backend, item, config.protocol);
  const after = own ? await state(backend) : null;
  if (own) Object.assign(result, measuredUsage(before, after));
  if (item.kind === "fetch") {
    await verify(client, source, want, git);
    if (own) checkReadOnly(before, after, result.usage);
  } else if (own) await verifyPush(git, source, url, want, item.generation);
  return result;
}

async function trial(context, sample, record, checkpoint) {
  const { directory, config, git, bundle } = context;
  let oracle, own;
  try {
    oracle = { name: "git", ...(await reference(directory, repositoryRoot, git, config.objectFormat)), repository: join(directory, "repo.git") };
    own = { name: "own", persist: join(directory, "state") };
    own.server = await startRuntime(own.persist, bundle, config.runtime, Number(config.objectFormat === "sha256"));
    record.startup = await state(own);
    for (const item of schedule(config.generations)) {
      const row = { operation: item, backends: {} }; record.operations.push(row);
      for (const backend of sample % 2 ? [oracle, own] : [own, oracle]) {
        row.backends[backend.name] = await operation(context, item, backend);
        await checkpoint();
      }
      process.stdout.write(JSON.stringify({ sample, operation: record.operations.length, group: item.group }) + "\n");
    }
  } finally {
    const outcomes = await Promise.allSettled([own?.server?.stop(), oracle?.stop()]);
    for (const outcome of outcomes) if (outcome.status === "rejected") throw outcome.reason;
  }
}

function summary(trials) {
  const groups = new Map();
  for (const row of trials.flatMap((trial) => trial.operations)) {
    const values = groups.get(row.operation.group) ?? [];
    values.push(row.backends.own.readySeconds / row.backends.git.readySeconds); groups.set(row.operation.group, values);
  }
  return Object.fromEntries([...groups].map(([name, values]) => {
    values.sort((a, b) => a - b);
    return [name, { observations: values.length, medianOwnToGitReady: (values[Math.floor((values.length - 1) / 2)] + values[Math.floor(values.length / 2)]) / 2,
      p95OwnToGitReady: values[Math.ceil(values.length * .95) - 1] }];
  }));
}

async function main(config) {
  await mkdir(config.workDir); // Never overwrite existing evidence.
  const git = new Git(config.git, controller.signal), source = join(config.workDir, "source.git");
  const report = { schemaVersion: 1, status: "running", identity: { ...config, seed: String(SEED), referenceMaintenance: MAINTENANCE }, trials: [], unknownBillingDimensions: UNKNOWN_BILLING,
    limits: ["Local emulator timings are not deployed Cloudflare latency or billed CPU.",
      "The reference uses Git CGI through a threaded Python HTTP adapter; local frontend overheads differ.",
      "Cold restarts the own runtime before each fetch; OS page caches remain uncontrolled.",
      "Client seeding, integrity/checkout verification and observer requests are outside wire timers.",
      "R2 write bytes count attempted uploads; read bytes count complete bodies, not partial failed transfers.",
      "Screening cannot establish a performance win; full runs retain raw pairs for independent analysis."] };
  try {
    Object.assign(report.identity, { gitVersion: await git.text(repositoryRoot, ["--version"]), codecCommit: await git.text(repositoryRoot, ["rev-parse", "HEAD"]),
      workingTreeClean: !(await git.text(repositoryRoot, ["status", "--porcelain"])) });
    const tips = await fixture(source, git, config); report.identity.tips = tips;
    const bundle = join(config.workDir, "frozen");
    report.identity.bundle = await freezeRuntime(bundle, config.runtime, Number(config.objectFormat === "sha256"), { signal: controller.signal });
    await save(config.workDir, report);
    for (let sample = 0; sample < config.samples; sample++) {
      const directory = join(config.workDir, "pair-" + sample); await mkdir(directory);
      const record = { sample, operations: [] }; report.trials.push(record);
      await trial({ directory, source, tips, config, git, bundle }, sample, record, () => save(config.workDir, report));
    }
    report.summary = summary(report.trials); report.status = "complete";
  } catch (error) { report.status = "failed"; report.failure = error.message; throw error; }
  finally { await save(config.workDir, report); }
}

await main(options(process.argv.slice(2)));
