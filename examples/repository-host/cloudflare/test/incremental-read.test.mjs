import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { createServer } from "node:http";
import { Readable } from "node:stream";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { git as oracle, receive } from "./browse-fixture.mjs";
import { pack, packet, receiveReport } from "./fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));

for (const format of [0, 1]) test(`real Git incremental pull pins many small fallbacks before first byte and denies rewritten wants format=${format}`, { timeout: 60000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-incremental-")), db = new DatabaseSync(":memory:"), objects = new Map();
  let temporaryPuts = 0, reads = 0; const samples = [];
  const factory = createRepositoryHost({ codec, authMode: "gateway", bindings: { repositories: "R", objects: "B", grantKey: "K" }, routePrefix: "", keyPrefix: "" });
  const env = { K: GRANT_KEY, B: { async get(key) { reads++; const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { if (key.includes("/tmp/")) temporaryPuts++; objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); } } };
  const host = new factory.Repository(sqliteContext(db), env);
  const call = (route, method, body, level = "admin") => host.fetch(new Request(`http://host/repo.git/${route}`, { method, headers: { "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", level), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, ...(body === undefined ? {} : { body }) }));
  const server = createServer(async (request, response) => {
    try {
      const started = Date.now(), beforeReads = reads, beforePuts = temporaryPuts;
      const result = await host.fetch(new Request(`http://host${request.url}`, { method: request.method, headers: { ...request.headers, "x-git-zero-namespace": "repo", "x-git-zero-grant": mint("repo", "read") }, ...(["GET", "HEAD"].includes(request.method) ? {} : { body: Readable.toWeb(request), duplex: "half" }) }));
      const fallbackCount = Math.max(0, ...[...host.jobs.values()].map((job) => job.preparedFallbacks ?? 0));
      response.writeHead(result.status, Object.fromEntries(result.headers));
      if (result.body) {
        const body = Readable.fromWeb(result.body);
        body.once("data", () => { if (request.method === "POST") samples.push({ firstByteMs: Date.now() - started, r2Gets: reads - beforeReads, temporaryPuts: temporaryPuts - beforePuts, fallbackCount }); });
        body.on("error", (error) => response.destroy(error)).pipe(response);
      } else response.end();
    } catch (error) { response.writeHead(500); response.end(String(error)); }
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  async function git(...args) {
    const child = spawn("git", args, { cwd: directory, timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
    let stdout = "", stderr = ""; child.stdout.on("data", (v) => { stdout += v; }); child.stderr.on("data", (v) => { stderr += v; }); const [status] = await once(child, "exit"); return { status, stdout, stderr };
  }
  const pushed = async (updates, data) => { const response = await call("git-receive-pack", "POST", receive(format, updates, data), "write"); assert.equal(response.status, 200); assert.doesNotMatch(receiveReport(await response.arrayBuffer()), /ng /); await host.queue; };
  try {
    assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }))).status, 201);
    const source = join(directory, "source"); await mkdir(source); await mkdir(join(source, "src/framework/models"), { recursive: true });
    oracle(source, ["init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`]);
    const content = (i) => Array.from({ length: 64 }, (_, n) => createHash("sha256").update(`${i}:${n}`).digest("hex")).join("\n");
    for (let i = 0; i < 80; i++) await writeFile(join(source, `src/framework/models/file-${i}.py`), content(i) + "\nrevision 1\n");
    oracle(source, ["add", "."]); oracle(source, ["commit", "-m", "initial framework"]);
    const first = oracle(source, ["rev-parse", "HEAD"]).toString().trim();
    await pushed([{ name: "refs/heads/main", old: "0".repeat(format ? 64 : 40), new: first }], oracle(source, ["pack-objects", "--stdout", "--all", "--delta-base-offset"]));
    const remote = `http://127.0.0.1:${server.address().port}/repo.git`, clone = await git("clone", "--quiet", remote, "client"); assert.equal(clone.status, 0, clone.stderr);
    for (let i = 0; i < 80; i++) await writeFile(join(source, `src/framework/models/file-${i}.py`), content(i) + "\nrevision 2\n");
    oracle(source, ["add", "."]); oracle(source, ["commit", "-m", "updated framework"]);
    const second = oracle(source, ["rev-parse", "HEAD"]).toString().trim();
    await pushed([{ name: "refs/heads/main", old: first, new: second }], oracle(source, ["pack-objects", "--stdout", "--revs", "--thin", "--delta-base-offset"], `${second}\n^${first}\n`));
    samples.length = 0;
    const fetched = await git("-C", "client", "fetch", "--quiet", "origin"); assert.equal(fetched.status, 0, fetched.stderr);
    const increment = samples.find((sample) => sample.fallbackCount >= 40); assert.ok(increment, JSON.stringify(samples));
    assert.equal(increment.temporaryPuts, 0); assert.ok(increment.r2Gets <= 8, JSON.stringify(increment)); assert.ok(increment.firstByteMs < 5000, JSON.stringify(increment));
    t.diagnostic(JSON.stringify({ format, ...increment }));
    // A tiny entry budget spills otherwise byte-small fallbacks instead of
    // retaining an unbounded collection of tiny buffer/key wrappers.
    host.env.READ_PIN_ENTRIES = 1;
    const beforeSpill = temporaryPuts;
    const spilling = await call("git-upload-pack", "POST", Buffer.concat([packet(`want ${second} side-band-64k ofs-delta\n`), Buffer.from("0000"), packet(`have ${first}\n`), packet("done\n")]), "read");
    assert.equal(spilling.status, 200); await spilling.arrayBuffer(); await host.queue;
    assert.ok(temporaryPuts - beforeSpill >= 40 && temporaryPuts - beforeSpill <= 80);
    assert.equal(host.store.exec("SELECT bytes FROM read_scratch_usage")[0].bytes, 0);
    host.env.READ_PIN_ENTRIES = 4096;
    assert.equal((await git("-C", "client", "rev-parse", "origin/main")).stdout.trim(), second);
    // Reachable ancestry remains explicitly fetchable. Once all current refs
    // are replaced, the same Git request gets an ERR rather than old content.
    assert.equal((await git("init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, "ancestor.git")).status, 0);
    const ancestor = await git("-C", "ancestor.git", "fetch", remote, first); assert.equal(ancestor.status, 0, ancestor.stderr);
    await pushed([{ name: "refs/heads/main", old: second, new: "0".repeat(format ? 64 : 40) }], pack(format, []));
    const unrelated = oracle(source, ["commit-tree", "HEAD^{tree}", "-m", "new root"]).toString().trim();
    await pushed([{ name: "refs/heads/main", old: "0".repeat(format ? 64 : 40), new: unrelated }], oracle(source, ["pack-objects", "--stdout"], `${unrelated}\n`));
    assert.equal((await git("init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, "denied.git")).status, 0);
    const denied = await git("-C", "denied.git", "fetch", remote, first); assert.notEqual(denied.status, 0); assert.match(denied.stderr, /not reachable from current refs/);
    const log = await (await call("api/v1/changes", "GET")).json(); assert.ok(log.publications.some((event) => event.updates.some((update) => update.oldId === second)));
    assert.ok(host.store.published(first), "physical bytes remain until explicit reclamation/deletion");
    assert.equal(host.env.BYTE_BUDGET.used, host.env.BYTE_BUDGET.residentBytes);
  } finally { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); await host.queue; db.close(); await rm(directory, { recursive: true, force: true }); }
});
