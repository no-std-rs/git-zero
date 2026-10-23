// Git is the independent client/oracle. Own hosting uses the streamed Wasm host.
import { execFile, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { closeSync, openSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { promisify } from "node:util";

const execute = promisify(execFile);
export const MAINTENANCE = [["multi-pack-index", "write", "--bitmap"], ["commit-graph", "write", "--reachable", "--split"]];

export class Git {
  constructor(binary, signal) {
    this.binary = binary; this.signal = signal;
    this.env = { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_"))),
      GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0", LC_ALL: "C", TZ: "UTC",
      GIT_AUTHOR_NAME: "Fixture", GIT_AUTHOR_EMAIL: "fixture@example.invalid", GIT_COMMITTER_NAME: "Fixture", GIT_COMMITTER_EMAIL: "fixture@example.invalid",
      GIT_AUTHOR_DATE: "2000-01-01T00:00:00Z", GIT_COMMITTER_DATE: "2000-01-01T00:00:00Z" };
  }
  async run(directory, args, input = null, allowFailure = false) {
    const pending = execute(this.binary, ["-c", "http.postBuffer=134217728", "-c", "pack.threads=1", "-c", "index.threads=1", "-C", directory, ...args],
      { env: this.env, encoding: "buffer", maxBuffer: 128 * 1024 * 1024, timeout: 120000, signal: this.signal });
    pending.child.stdin.on("error", () => {}); pending.child.stdin.end(input);
    try { return { ...(await pending), code: 0 }; }
    catch (error) { if (allowFailure && Number.isInteger(error.code)) return error; throw error; }
  }
  async text(directory, args, input = null) { return (await this.run(directory, args, input)).stdout.toString().trim(); }
}

function payload(size, seed) {
  const result = Buffer.alloc(size);
  for (let offset = 0; offset < size; offset += 32) {
    const block = createHash("sha256").update(seed + ":" + offset).digest();
    block.copy(result, offset, 0, Math.min(32, size - offset));
  }
  return result;
}

export async function fixture(source, git, options) {
  await mkdir(source);
  await git.run(source, ["init", "--bare", "--initial-branch=main", "--object-format=" + options.objectFormat]);
  const tips = []; let large;
  for (let generation = 1; generation <= options.generations; generation++) {
    if (generation === 1 || generation % 4 === 1) large = await git.text(source, ["hash-object", "-w", "--stdin"], payload(options.blobBytes, generation));
    const small = await git.text(source, ["hash-object", "-w", "--stdin"], generation + "\n");
    const tree = await git.text(source, ["mktree"], "100644 blob " + small + "\tgeneration.txt\n100644 blob " + large + "\tpayload.bin\n");
    tips.push(await git.text(source, ["commit-tree", tree, ...(tips.length ? ["-p", tips.at(-1)] : [])], "generation " + generation + "\n"));
  }
  await git.run(source, ["update-ref", "refs/heads/main", tips.at(-1)]);
  await git.run(source, ["fsck", "--strict"]);
  return tips;
}

export async function seedClient(client, source, have, want, git, format) {
  await mkdir(client);
  await git.run(client, ["init", "--bare", "--initial-branch=result", "--object-format=" + format]);
  if (have) await git.run(client, ["fetch", "--no-tags", source, have + ":refs/heads/seed"]);
  if ((await git.run(client, ["cat-file", "-e", want], null, true)).code === 0) throw new Error("fetch client already has its wanted commit");
}

export async function verify(client, source, want, git) {
  if (await git.text(client, ["rev-parse", "refs/heads/result"]) !== want) throw new Error("fetched ref differs from requested generation");
  await git.run(client, ["fsck", "--strict"]);
  for (const args of [["rev-list", "--objects", "--no-object-names", want], ["archive", want]]) {
    let actual = (await git.run(client, args)).stdout, expected = (await git.run(source, args)).stdout;
    if (args[0] === "rev-list") {
      actual = Buffer.from(actual.toString().split("\n").sort().join("\n"));
      expected = Buffer.from(expected.toString().split("\n").sort().join("\n"));
    }
    if (!actual.equals(expected)) throw new Error("fetched history or checkout differs from source");
  }
}

export async function reference(directory, repositoryRoot, git, format) {
  await git.run(directory, ["init", "--bare", "--initial-branch=main", "--object-format=" + format, "repo.git"]);
  for (const [key, value] of [["http.receivepack", "true"], ["receive.unpackLimit", "0"], ["pack.threads", "1"], ["pack.allowPackReuse", "multi"]]) {
    await git.run(join(directory, "repo.git"), ["config", key, value]);
  }
  const log = openSync(join(directory, "reference.log"), "wx");
  let child;
  try { child = spawn("python3", [join(repositoryRoot, "tests/hosting_reference.py"), directory, "--git", git.binary], { stdio: ["pipe", "pipe", log] }); }
  finally { closeSync(log); }
  const lines = createInterface({ input: child.stdout });
  let timeout;
  try {
    const [line] = await Promise.race([once(lines, "line"), once(child, "error").then(([error]) => { throw error; }),
      once(child, "exit").then(() => { throw new Error("reference exited before startup; inspect reference.log"); }),
      new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error("reference startup timed out")), 30000); })]);
    return { origin: JSON.parse(line).origin, async stop() {
      if (child.exitCode !== null) throw new Error("reference exited unexpectedly");
      const exit = once(child, "exit"); child.stdin.end();
      const timer = setTimeout(() => child.kill("SIGKILL"), 10000);
      try { const [code] = await exit; if (code !== 0) throw new Error("reference did not stop cleanly"); }
      finally { clearTimeout(timer); lines.close(); }
    } };
  } catch (error) { child.kill(); lines.close(); throw error; }
  finally { clearTimeout(timeout); }
}
