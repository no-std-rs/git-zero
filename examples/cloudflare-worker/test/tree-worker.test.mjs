import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { createWorker } from "../src/worker.mjs";
import { withServer } from "./http.mjs";
import { git as oracleGit, gitEnvironment, oracle } from "./tree-fixture.mjs";

const execute = promisify(execFile);
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
const json = (value) => JSON.stringify(value, null, 2) + "\n";
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const credential = "Access-assertion-must-remain-private";

function fixture(backend = "inline") {
  const records = new Map();
  const state = { writes: 0, reads: 0 };
  const store = {
    async put(key, value, options) {
      state.writes++;
      await state.beforeWrite?.();
      if (state.failWrite) throw new Error("private storage failure");
      const bytes = new Uint8Array(value).slice();
      assert.equal(key, `snapshots/v2/${hash(bytes)}`);
      assert.equal(JSON.parse(new TextDecoder().decode(bytes)).version, 2);
      assert.ok(!new TextDecoder().decode(bytes).includes(credential));
      if (backend === "kv") assert.equal(options.expirationTtl, 86400);
      else assert.equal(options.sha256, hash(bytes));
      records.set(key, bytes);
    },
    async get(key, type) {
      state.reads++;
      if (state.failRead) throw new Error("private storage failure");
      assert.equal(type, backend === "kv" ? "stream" : undefined);
      const bytes = records.get(key);
      if (!bytes) return null;
      const stream = new Blob([bytes]).stream();
      return backend === "kv" ? stream : { body: stream };
    },
  };
  const env = { SNAPSHOT_LAYOUT: "tree", SNAPSHOT_STORAGE: backend,
    [backend === "kv" ? "SNAPSHOTS_KV" : "SNAPSHOTS_R2"]: store,
    EXPOSED_VARS: "PUBLIC,PUBLIC_JSON,A,a", PUBLIC: "visible", PUBLIC_JSON: { enabled: true }, A: "uppercase", a: "lowercase", PRIVATE: credential };
  const request = (origin) => {
    const result = new Request(origin + "/", { headers: { "cf-ray": "public-CDG", "cf-access-jwt-assertion": credential } });
    Object.defineProperty(result, "cf", { value: { colo: "CDG", nested: { rtt: 7 } } });
    return result;
  };
  const files = new Map([
    ["cf/colo.json", json("CDG")], ["cf/nested.json", json({ rtt: 7 })], ["headers/cf-ray.json", json("public-CDG")],
    ["vars/%50%55%42%4C%49%43.json", json("visible")], ["vars/%50%55%42%4C%49%43_%4A%53%4F%4E.json", json({ enabled: true })],
    ["vars/%41.json", json("uppercase")], ["vars/a.json", json("lowercase")],
    ["runtime/platform.json", json("Cloudflare Workers")], ["runtime/format.json", json(2)],
    ["runtime/history.json", json("one root commit; no parents")], ["runtime/layout.json", json("tree")],
  ]);
  return { env, request, files, records, state };
}

for (const backend of ["inline", "kv", "r2"]) {
  test(`real Git clones immutable ${backend} tree snapshots against an independent Git oracle`, async () => {
    const source = fixture(backend);
    const directory = await mkdtemp(join(tmpdir(), `git-zero-tree-${backend}-`));
    const reference = join(directory, "reference.git");
    const expectedHead = oracle(reference, source.files);
    try {
      await withServer(async (request) => createWorker(module).fetch(request, source.env), async (origin) => {
        const capture = await createWorker(module).fetch(source.request(origin), source.env);
        assert.equal(capture.status, 200);
        const snapshot = await capture.json();
        assert.equal(snapshot.layout, "tree");
        assert.deepEqual(snapshot.files, [...source.files.keys()].sort());
        assert.match(snapshot.clone_url, backend === "inline" ? /\/r\/v2\// : new RegExp(`/s/v2/${backend}/[a-f0-9]{64}/repo.git$`));
        if (backend === "inline") {
          const payload = Buffer.from(new URL(snapshot.clone_url).pathname.split("/")[3], "base64url").toString();
          assert.ok(!payload.includes(credential));
          assert.equal(JSON.parse(payload).version, 2);
        }
        // Current configuration/data cannot reinterpret a previously captured URL.
        source.env.SNAPSHOT_LAYOUT = "flat";
        source.env.PUBLIC = "changed";
        source.env.PUBLIC_JSON = { changed: true };
        const advertised = await Promise.all(Array.from({ length: 6 }, () => createWorker(module).fetch(new Request(snapshot.clone_url + "/info/refs"), source.env).then((r) => r.text())));
        assert.ok(advertised.every((body) => body.includes(expectedHead)));
        for (const version of [0, 1, 2, "dumb"]) {
          const git = (...args) => execute(process.env.GIT_ZERO_TEST_GIT ?? "git", ["-c", `protocol.version=${version === "dumb" ? 0 : version}`, ...args], { env: gitEnvironment({ dumb: version === "dumb" }), encoding: null, timeout: 60000 });
          const clone = join(directory, String(version));
          await git("clone", "-q", snapshot.clone_url, clone);
          await git("-C", clone, "fsck", "--strict");
          assert.equal((await git("-C", clone, "rev-parse", "HEAD")).stdout.toString().trim(), expectedHead);
          const ids = (await git("-C", clone, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)")).stdout.toString().trim().split("\n").sort();
          const expectedIds = oracleGit(reference, ["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]).toString().trim().split("\n").sort();
          assert.deepEqual(ids, expectedIds);
          for (const [path, body] of source.files) assert.equal(await readFile(join(clone, path), "utf8"), body);
          for (const id of ids) {
            const kind = oracleGit(reference, ["cat-file", "-t", id]).toString().trim();
            const body = (await git("-C", clone, "cat-file", kind, id)).stdout;
            assert.deepEqual(body, oracleGit(reference, ["cat-file", kind, id]));
            assert.ok(!body.includes(Buffer.from(credential)));
          }
          await git("-C", clone, "fetch", "origin");
          await git("-C", clone, "fsck", "--strict");
        }
      });
    } finally { await rm(directory, { recursive: true, force: true }); }
  });
}

test("oversized request frames retain 413 for inline, KV and R2 tree URLs", async () => {
  for (const backend of ["inline", "kv", "r2"]) {
    const source = fixture(backend), handler = createWorker(module);
    const snapshot = await (await handler.fetch(source.request("https://demo.example"), source.env)).json();
    for (const [path, options] of [
      ["/HEAD?padding=" + "x".repeat(65536), {}],
      ["/HEAD", { headers: { "git-protocol": "x".repeat(65536) } }],
      ["/git-upload-pack?padding=" + "x".repeat(32768), { method: "POST", body: "x".repeat(32768), headers: { "Content-Type": "application/x-git-upload-pack-request" } }],
    ]) {
      const response = await handler.fetch(new Request(snapshot.clone_url + path, options), source.env);
      assert.equal(response.status, 413, backend);
      assert.equal(response.headers.get("Retry-After"), null);
      assert.equal(await (await handler.fetch(new Request(snapshot.clone_url + "/HEAD"), source.env)).text(), "ref: refs/heads/main\n");
    }
  }
});

test("tree capture validates names, file count and public selections before storage writes", async () => {
  for (const change of [
    (env) => { env.EXPOSED_CF_HEADERS = "cf-access-jwt-assertion"; },
    (env) => { env.EXPOSED_VARS = "PUBLIC"; Object.defineProperty(env, "PUBLIC", { get() { throw new Error("getter must never run"); } }); },
    (env) => { env.SNAPSHOT_LAYOUT = "unknown"; },
    (env) => { env.PUBLIC_JSON = new Date(); },
  ]) {
    const source = fixture("kv"); change(source.env);
    const response = await createWorker(module).fetch(source.request("https://demo.example"), source.env);
    assert.equal(response.status, 503);
    assert.equal(source.state.writes, 0);
  }
  for (const cf of [{ ["a".repeat(251)]: true }, Object.fromEntries(Array.from({ length: 56 }, (_, i) => [`v${i}`, i])), { padding: "p".repeat(8192) }]) {
    const source = fixture("r2");
    const request = new Request("https://demo.example/"); Object.defineProperty(request, "cf", { value: cf });
    assert.equal((await createWorker(module).fetch(request, source.env)).status, 413);
    assert.equal(source.state.writes, 0);
  }
});

for (const backend of ["kv", "r2"]) {
  test(`${backend} v2 records reject schema confusion, corruption, expiry and backend failures`, async () => {
    const source = fixture(backend);
    const handler = () => createWorker(module);
    const capture = await handler().fetch(source.request("https://demo.example"), source.env);
    const snapshot = await capture.json();
    const target = snapshot.clone_url + "/HEAD";
    const fetch = () => handler().fetch(new Request(target), source.env);
    const [key, bytes] = [...source.records][0];
    const record = JSON.parse(new TextDecoder().decode(bytes));
    const prepared = await fetch();
    const cancelled = await fetch(); await cancelled.body.cancel();
    source.records.delete(key);
    assert.equal((await fetch()).status, 404);
    assert.match(await prepared.text(), /^ref: refs\/heads\/main\n$/);
    source.records.set(key, bytes);
    source.state.failRead = true;
    const unavailable = await fetch(); assert.equal(unavailable.status, 503); assert.ok(!(await unavailable.text()).includes("private"));
    source.state.failRead = false;
    for (const corrupt of [new Uint8Array(9000), new TextEncoder().encode("corrupt"), bytes.slice(0, -1)]) {
      source.records.set(key, corrupt);
      assert.equal((await fetch()).status, 502);
    }
    source.records.set(key, bytes);
    for (const invalid of [
      { ...record, version: 1 }, { ...record, layout: "flat" }, { ...record, format: "sha256" },
      { ...record, expiresAt: "later" }, { ...record, parts: ["a", "b", "c", "d"] },
      { ...record, parts: [json({ ["a".repeat(251)]: true }), ...record.parts.slice(1)] },
    ]) {
      const invalidBytes = new TextEncoder().encode(JSON.stringify(invalid)), id = hash(invalidBytes);
      source.records.set(`snapshots/v2/${id}`, invalidBytes);
      const response = await handler().fetch(new Request(`https://demo.example/s/v2/${backend}/${id}/repo.git/HEAD`), source.env);
      assert.equal(response.status, 502);
    }
    const expired = new TextEncoder().encode(JSON.stringify({ ...record, expiresAt: 0 })), id = hash(expired);
    source.records.set(`snapshots/v2/${id}`, expired);
    assert.equal((await handler().fetch(new Request(`https://demo.example/s/v2/${backend}/${id}/repo.git/HEAD`), source.env)).status, 410);
    source.records.set(`snapshots/v1/${id}`, expired);
    assert.equal((await handler().fetch(new Request(`https://demo.example/s/${backend}/${id}/repo.git/HEAD`), source.env)).status, 502);
    source.state.failWrite = true;
    const failedCapture = await handler().fetch(source.request("https://demo.example"), source.env);
    assert.equal(failedCapture.status, 503);
    assert.ok(!(await failedCapture.text()).includes("private"));
  });

  test(`${backend} tree publication waits for storage and overlapping captures stay independent`, async () => {
    const source = fixture(backend), pending = [];
    source.state.beforeWrite = () => new Promise((resolve) => pending.push(resolve));
    let published = 0;
    const first = createWorker(module).fetch(source.request("https://demo.example"), source.env).then(async (response) => { published++; return response.json(); });
    while (pending.length < 1) await new Promise(setImmediate);
    source.env.PUBLIC = "second capture";
    const second = createWorker(module).fetch(source.request("https://demo.example"), source.env).then(async (response) => { published++; return response.json(); });
    while (pending.length < 2) await new Promise(setImmediate);
    assert.equal(published, 0); assert.equal(source.records.size, 0);
    pending[1](); const secondSnapshot = await second;
    pending[0](); const firstSnapshot = await first;
    assert.notEqual(firstSnapshot.clone_url, secondSnapshot.clone_url);
    assert.equal(source.records.size, 2);
    const values = [...source.records.values()].map((bytes) => JSON.parse(new TextDecoder().decode(bytes))).map((record) => JSON.parse(record.parts[2]).PUBLIC).sort();
    assert.deepEqual(values, ["second capture", "visible"]);
    const refs = await Promise.all([firstSnapshot, secondSnapshot].map(async (snapshot) =>
      (await createWorker(module).fetch(new Request(snapshot.clone_url + "/info/refs"), source.env)).text()));
    assert.notEqual(refs[0], refs[1]);
  });
}

test("inline versions retain separate schemas and layout changes preserve old flat IDs", async () => {
  const source = fixture(); source.env.SNAPSHOT_LAYOUT = "flat";
  const handler = createWorker(module);
  const flat = await (await handler.fetch(source.request("https://demo.example"), source.env)).json();
  const original = await (await handler.fetch(new Request(flat.clone_url + "/info/refs"))).text();
  source.env.SNAPSHOT_LAYOUT = "tree";
  assert.equal(await (await handler.fetch(new Request(flat.clone_url + "/info/refs"), source.env)).text(), original);
  const tree = await (await handler.fetch(source.request("https://demo.example"), source.env)).json();
  const payload = JSON.parse(Buffer.from(new URL(tree.clone_url).pathname.split("/")[3], "base64url").toString());
  for (const invalid of [{ ...payload, version: 1 }, { ...payload, layout: "flat" }, { ...payload, format: "sha256" }, payload.parts]) {
    const token = Buffer.from(JSON.stringify(invalid)).toString("base64url");
    assert.equal((await handler.fetch(new Request(`https://demo.example/r/v2/${token}/repo.git/HEAD`))).status, 400);
  }
  const token = Buffer.from(JSON.stringify(payload)).toString("base64url");
  assert.equal((await handler.fetch(new Request(`https://demo.example/r/${token}/repo.git/HEAD`))).status, 400);
  const flatAgain = await (await handler.fetch(source.request("https://demo.example"), { ...source.env, SNAPSHOT_LAYOUT: "flat" })).json();
  assert.equal(flatAgain.clone_url, flat.clone_url);
});
