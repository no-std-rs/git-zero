import assert from "node:assert/strict";
import { lstat, readFile, readlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { gzipSync, inflateSync } from "node:zlib";
import { createRepositoryWorker } from "../src/repository.mjs";
import { withServer } from "./http.mjs";
import { directory, execute, fixtures, gitBinary, gitEnvironment, object } from "./repository-fixture.mjs";

const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
function worker(repository, observed = []) {
  return createRepositoryWorker(module, async (request) => {
    const url = new URL(request.url);
    const match = /^\/repo\.git\/(.*)$/.exec(url.pathname);
    if (!match) return null;
    observed.push({ method: request.method, protocol: request.headers.get("git-protocol"), bytes: Number(request.headers.get("content-length") ?? 0) });
    return { repository, target: `${match[1]}${url.search}` };
  });
}

function client(format, version) {
  const dumb = version === "dumb";
  return (...args) => execute(gitBinary, ["-c", `protocol.version=${dumb ? 0 : version}`, ...args], {
    env: gitEnvironment(format, dumb), encoding: "buffer", timeout: 60000,
  });
}

async function objectIds(git, clone) {
  const output = (await git("-C", clone, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)")).stdout;
  return output.toString().trim().split("\n").filter(Boolean).sort();
}

async function unassistedDumb(git, clone, repository) {
  const empty = repository.objects.find((value) => value.kind === "tree" && value.body.length === 0);
  const result = await git("-C", clone, "fsck", "--strict").then(value => ({ ...value, code: 0 }), error => error);
  const ids = await objectIds(git, clone);
  // Git's loose-dumb walker skips the canonical empty tree. Keep the missing
  // object in the expected inventory and compare the same Git static control.
  assert.equal(result.code, 2);
  assert.equal(result.stdout.toString().trim(), `missing tree ${empty.id}`);
  assert.equal(result.stderr.length, 0);
  const expected = new Set(repository.objects.map((value) => value.id));
  const imported = new Set(ids);
  assert.deepEqual([...expected].filter((id) => !imported.has(id)), [empty.id]);
  assert.deepEqual(ids.filter((id) => !expected.has(id)), []);
  return { expectedObjects: expected.size, objects: ids.length, missingObjects: [empty.id], fsckCode: result.code, fsckOutput: result.stdout.toString().trim() };
}

async function staticDumbControl(git, repository) {
  await git("-C", repository.source, "update-server-info");
  let result;
  await withServer(async (request) => {
    const match = /^\/repo\.git\/(HEAD|info\/refs|objects\/[0-9a-f]{2}\/[0-9a-f]+|objects\/info\/[a-z-]+)$/.exec(new URL(request.url).pathname);
    if (!match) return new Response("missing", { status: 404 });
    try { return new Response(await readFile(join(repository.source, match[1]))); }
    catch { return new Response("missing", { status: 404 }); }
  }, async (origin) => {
    const clone = join(directory, `static-control-${repository.format}`);
    await git("clone", "-q", `${origin}/repo.git`, clone);
    result = await unassistedDumb(git, clone, repository);
  });
  return result;
}

async function importEmptyTree(git, clone, repository, url) {
  const empty = repository.objects.find((value) => value.kind === "tree" && value.body.length === 0);
  const response = await fetch(`${url}/objects/${empty.id.slice(0, 2)}/${empty.id.slice(2)}`);
  assert.equal(response.status, 200);
  assert.match(response.headers.get("content-type"), /^application\/x-git-loose-object/);
  const loose = inflateSync(Buffer.from(await response.arrayBuffer()));
  assert.deepEqual(loose, Buffer.from("tree 0\0"));
  const body = loose.subarray(loose.indexOf(0) + 1);
  assert.equal(object(repository.format, "tree", body).id, empty.id);
  const file = join(directory, `empty-tree-${repository.format}`);
  await writeFile(file, body);
  assert.equal((await git("-C", clone, "hash-object", "-w", "-t", "tree", file)).stdout.toString().trim(), empty.id);
}

async function verifyClone(git, clone, repository) {
  await git("-C", clone, "fsck", "--strict");
  assert.equal((await git("-C", clone, "rev-parse", "HEAD")).stdout.toString().trim(), repository.head.id);
  assert.deepEqual(await objectIds(git, clone), repository.objects.map((value) => value.id).sort());
  for (const object of repository.objects) {
    assert.deepEqual((await git("-C", clone, "cat-file", object.kind, object.id)).stdout, object.body);
  }
  const treeArgs = ["ls-tree", "-r", "-z", "HEAD"];
  const expected = (await git("-C", repository.source, ...treeArgs)).stdout;
  assert.deepEqual((await git("-C", clone, ...treeArgs)).stdout, expected);
  const directories = ["ls-tree", "-d", "-r", "-z", "HEAD"];
  const expectedDirectories = (await git("-C", repository.source, ...directories)).stdout;
  assert.ok(expectedDirectories.includes(Buffer.from("nested/empty-dir")));
  assert.deepEqual((await git("-C", clone, ...directories)).stdout, expectedDirectories);
  for (const record of expected.toString().split("\0").filter(Boolean)) {
    const [metadata, path] = record.split("\t");
    const [mode, , id] = metadata.split(" ");
    const body = repository.objects.find((value) => value.id === id).body;
    const file = join(clone, path);
    const info = await lstat(file);
    if (mode === "120000") {
      assert.ok(info.isSymbolicLink());
      assert.equal(await readlink(file), body.toString());
    } else {
      assert.deepEqual(await readFile(file), body);
      assert.equal(Boolean(info.mode & 0o111), mode === "100755");
    }
  }
}

for (const repository of fixtures) {
  for (const version of [0, 1, 2, "dumb"]) {
    const label = version === "dumb" ? "loose-dumb matches Git control with explicit empty-tree import" : `v${version} clones and incrementally fetches all 44 objects`;
    test(`real Git ${repository.format} ${label}`, async (context) => {
      const git = client(repository.format, version);
      const observed = [];
      const handler = worker(repository, observed);
      await withServer((request) => handler.fetch(request), async (origin) => {
        const url = `${origin}/repo.git`;
        const remote = (await git("ls-remote", "--symref", url)).stdout.toString().trim().split("\n").sort();
        const oracle = (await git("ls-remote", "--symref", repository.source)).stdout.toString().trim().split("\n").sort();
        assert.deepEqual(remote, oracle, "refs, immediate symrefs, HEAD and nested tag peels must match Git");
        const clone = join(directory, `full-${repository.format}-${version}`);
        await git("clone", "-q", url, clone);
        if (version === "dumb") {
          const actual = await unassistedDumb(git, clone, repository);
          assert.deepEqual(actual, await staticDumbControl(git, repository));
          context.diagnostic(JSON.stringify({ format: repository.format, unassistedWorker: actual, unassistedGitStatic: actual, assistedExpectedObjects: 44 }));
          await importEmptyTree(git, clone, repository, url);
        }
        await verifyClone(git, clone, repository);
        const incremental = join(directory, `incremental-${repository.format}-${version}`);
        await git("clone", "-q", "--single-branch", "--no-tags", "--branch=main-history", url, incremental);
        const initial = await objectIds(git, incremental);
        assert.ok(initial.length < repository.objects.length);
        assert.ok(!initial.includes(repository.head.id));
        await git("-C", incremental, "fetch", "-q", "origin",
          "refs/heads/z-default:refs/remotes/origin/z-default",
          "refs/heads/side:refs/remotes/origin/side", "refs/tags/*:refs/tags/*");
        await git("-C", incremental, "checkout", "-q", "--detach", repository.head.id);
        if (version === "dumb") {
          await unassistedDumb(git, incremental, repository);
          await importEmptyTree(git, incremental, repository, url);
        }
        await verifyClone(git, incremental, repository);
      });
      assert.ok(observed.length > 0);
      assert.ok(observed.every((request) => request.bytes <= 32768));
      if (version !== "dumb") assert.ok(observed.some((request) => request.method === "POST"));
      if (version === 2) assert.ok(observed.some((request) => request.protocol === "version=2"));
    });
  }
}

const packet = (line) => `${(Buffer.byteLength(line) + 4).toString(16).padStart(4, "0")}${line}`;
function fetchRequest(repository) {
  const body = packet("command=fetch\n") + packet(`object-format=${repository.format}\n`) + "0001" + packet(`want ${repository.head.id}\n`) + packet("done\n") + "0000";
  return new Request("https://demo.example/repo.git/git-upload-pack", {
    method: "POST", headers: { "Git-Protocol": "version=2", "Content-Type": "application/x-git-upload-pack-request" }, body,
  });
}

test("overlapping large-object streams own bytes across pauses, cancellation and source mutation", async () => {
  const repository = fixtures[0];
  const handler = worker(repository);
  const first = await handler.fetch(fetchRequest(repository));
  const second = await handler.fetch(fetchRequest(repository));
  assert.equal(first.status, 200);
  const reader = first.body.getReader();
  const prefix = await reader.read();
  assert.equal(prefix.done, false);
  const retained = Buffer.from(prefix.value);
  const expected = Buffer.from(await second.arrayBuffer());
  assert.ok(expected.length > 98304);
  const binary = repository.objects.find((value) => value.body.length === 98304).body;
  const original = binary[0];
  binary[0] ^= 0xff;
  const remaining = [];
  while (true) {
    const next = await reader.read();
    if (next.done) break;
    remaining.push(Buffer.from(next.value));
  }
  assert.deepEqual(Buffer.from(prefix.value), retained, "queued chunks survive later Rust output reuse");
  assert.deepEqual(Buffer.concat([retained, ...remaining]), expected, "imported body is immutable while a response streams");
  assert.equal((await handler.fetch(fetchRequest(repository))).status, 502, "future captures authenticate changed source bytes");
  binary[0] = original;
  const cancelled = await handler.fetch(fetchRequest(repository));
  const cancelReader = cancelled.body.getReader();
  await cancelReader.read();
  await cancelReader.cancel();
  assert.deepEqual(Buffer.from(await (await handler.fetch(fetchRequest(repository))).arrayBuffer()), expected);
});

test("repository hosts retain request limits, explicit failures and gzip decoding", async () => {
  const repository = fixtures[1];
  const handler = worker(repository);
  const request = fetchRequest(repository);
  const body = await request.clone().arrayBuffer();
  const expected = await (await handler.fetch(request)).arrayBuffer();
  const compressed = new Request(request.url, { method: "POST", headers: { ...Object.fromEntries(request.headers), "Content-Encoding": "gzip" }, body: gzipSync(Buffer.from(body)) });
  assert.deepEqual(Buffer.from(await (await handler.fetch(compressed)).arrayBuffer()), Buffer.from(expected));
  assert.equal((await handler.fetch(new Request(request.url, { method: "POST", body: new Uint8Array(32769) }))).status, 400);
  const bomb = new Request(request.url, { method: "POST", headers: { "Content-Encoding": "gzip" }, body: gzipSync(new Uint8Array(32769)) });
  assert.equal((await handler.fetch(bomb)).status, 400);
  const wrong = fetchRequest(repository);
  wrong.headers.set("Content-Type", "text/plain");
  assert.equal((await handler.fetch(wrong)).status, 415);
  assert.equal((await handler.fetch(new Request(request.url, { method: "DELETE" }))).status, 405);
  assert.equal((await handler.fetch(new Request("https://demo.example/missing"))).status, 404);
  const unavailable = createRepositoryWorker(module, async () => { throw new Error("private provider detail"); });
  const failure = await unavailable.fetch(new Request(request.url));
  assert.equal(failure.status, 503);
  assert.ok(!(await failure.text()).includes("private"));
  const overflow = worker({ ...repository, objects: Array(65).fill(repository.objects[0]) });
  assert.equal((await overflow.fetch(new Request(request.url))).status, 413);
});
