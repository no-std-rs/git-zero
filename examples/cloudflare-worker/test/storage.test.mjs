import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { test } from "node:test";
import { promisify } from "node:util";
import { createWorker } from "../src/worker.mjs";

const execute = promisify(execFile);
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
const encoder = new TextEncoder();
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");

function fixture(backend) {
  const records = new Map();
  const expirations = new Map();
  const state = { reads: 0, writes: 0, cancelled: 0, offsetMs: 0 };
  const store = {
    async put(key, value, options) {
      state.writes++;
      await state.beforeWrite?.();
      if (state.failWrite) throw new Error("private storage failure");
      const bytes = new Uint8Array(value).slice();
      assert.equal(key, `snapshots/v1/${hash(bytes)}`);
      if (backend === "kv") {
        assert.equal(options.expirationTtl, 86400);
        expirations.set(key, Date.now() + state.offsetMs + options.expirationTtl * 1000);
      }
      else assert.equal(options.sha256, hash(bytes));
      records.set(key, bytes);
      return backend === "kv" ? undefined : { key };
    },
    async get(key, type) {
      state.reads++;
      assert.equal(type, backend === "kv" ? "stream" : undefined);
      if (state.failRead) throw new Error("private storage failure");
      if (backend === "kv" && expirations.get(key) <= Date.now() + state.offsetMs) records.delete(key);
      const bytes = records.get(key);
      if (!bytes) return null;
      let offset = 0;
      const body = new ReadableStream({
        async pull(controller) {
          await Promise.resolve();
          if (state.failStream) return controller.error(new Error("private stream failure"));
          if (offset === bytes.length) return controller.close();
          const end = Math.min(offset + 127, bytes.length);
          controller.enqueue(bytes.slice(offset, end));
          offset = end;
        },
        cancel() { state.cancelled++; },
      }, { highWaterMark: 0 });
      return backend === "kv" ? body : { body, size: bytes.length };
    },
  };
  const env = { SNAPSHOT_STORAGE: backend, [backend === "kv" ? "SNAPSHOTS_KV" : "SNAPSHOTS_R2"]: store, EXPOSED_VARS: "PUBLIC", PUBLIC: { message: "immutable" } };
  return { env, records, state };
}

function request(origin) {
  const value = new Request(`${origin}/`, { headers: { "cf-ray": "stored-CDG" } });
  Object.defineProperty(value, "cf", { value: { colo: "CDG" } });
  return value;
}

async function capture(env, origin = "https://demo.example") {
  const response = await createWorker(module).fetch(request(origin), env);
  assert.equal(response.status, 200);
  return response.json();
}

for (const backend of ["kv", "r2"]) {
  test(`${backend} snapshots await publication and remain identical across isolates and concurrent readers`, async () => {
    const { env, records, state } = fixture(backend);
    let release;
    state.beforeWrite = () => new Promise((resolve) => { release = resolve; });
    let published = false;
    const pending = capture(env).then((value) => { published = true; return value; });
    while (!release) await new Promise((resolve) => setImmediate(resolve));
    assert.equal(published, false);
    assert.equal(records.size, 0);
    release();
    const snapshot = await pending;
    delete state.beforeWrite;
    assert.match(snapshot.clone_url, new RegExp(`/s/${backend}/[a-f0-9]{64}/repo.git$`));
    const expiry = Date.parse(snapshot.expires_at);
    assert.ok(expiry > Date.now() && expiry <= Date.now() + 86400000);
    const inline = await capture({ ...env, SNAPSHOT_STORAGE: "inline" });
    const expected = await (await createWorker(module).fetch(new Request(`${inline.clone_url}/info/refs?service=git-upload-pack`))).text();
    env.PUBLIC = { message: "changed after capture" };
    const responses = await Promise.all(Array.from({ length: 12 }, () => createWorker(module).fetch(new Request(`${snapshot.clone_url}/info/refs?service=git-upload-pack`), env)));
    assert.deepEqual(await Promise.all(responses.map((response) => response.text())), Array(12).fill(expected));
    const held = await createWorker(module).fetch(new Request(`${snapshot.clone_url}/info/refs?service=git-upload-pack`), env);
    const cancelled = await createWorker(module).fetch(new Request(`${snapshot.clone_url}/HEAD`), env);
    await cancelled.body.cancel();
    records.clear();
    // A prepared response owns its snapshot even if storage is evicted later.
    assert.equal(await held.text(), expected);
    assert.equal((await createWorker(module).fetch(new Request(`${snapshot.clone_url}/HEAD`), env)).status, 404);
  });

  test(`${backend} missing, corrupt, oversized, expired and failed storage never serves a different repository`, async () => {
    const { env, records, state } = fixture(backend);
    const snapshot = await capture(env);
    const target = `${snapshot.clone_url}/HEAD`;
    const fetch = () => createWorker(module).fetch(new Request(target), env);
    const [key, bytes] = [...records][0];
    state.failRead = true;
    assert.equal((await fetch()).status, 503);
    state.failRead = false;
    state.failStream = true;
    assert.equal((await fetch()).status, 502);
    state.failStream = false;
    records.set(key, encoder.encode("corrupt"));
    assert.equal((await fetch()).status, 502);
    records.set(key, new Uint8Array(9000));
    assert.equal((await fetch()).status, 502);
    assert.equal(state.cancelled, 1);
    records.delete(key);
    assert.equal((await fetch()).status, 404);
    records.set(key, bytes);
    assert.equal((await fetch()).status, 200);
    for (const invalid of [
      { version: 2, expiresAt: 9999999999, parts: ["a", "b", "c", "d"] },
      { version: 1, expiresAt: "later", parts: ["a", "b", "c", "d"] },
      { version: 1, expiresAt: 9999999999, parts: ["a", "b", "c", null] },
    ]) {
      const invalidBytes = encoder.encode(JSON.stringify(invalid));
      const id = hash(invalidBytes);
      records.set(`snapshots/v1/${id}`, invalidBytes);
      const response = await createWorker(module).fetch(new Request(`https://demo.example/s/${backend}/${id}/repo.git/HEAD`), env);
      assert.equal(response.status, 502);
    }
    const record = JSON.parse(new TextDecoder().decode(bytes));
    record.expiresAt = 1;
    const expired = encoder.encode(JSON.stringify(record));
    records.set(`snapshots/v1/${hash(expired)}`, expired);
    const expiredResponse = await createWorker(module).fetch(new Request(`https://demo.example/s/${backend}/${hash(expired)}/repo.git/HEAD`), env);
    assert.equal(expiredResponse.status, 410);
    state.failWrite = true;
    const failed = await createWorker(module).fetch(request("https://demo.example"), env);
    assert.equal(failed.status, 503);
    assert.ok(!(await failed.text()).includes("private"));
    assert.equal((await createWorker(module).fetch(request("https://demo.example"), { ...env, SNAPSHOT_TTL_SECONDS: 59 })).status, 503);
    assert.equal((await createWorker(module).fetch(request("https://demo.example"), { SNAPSHOT_STORAGE: backend })).status, 503);
  });

  test(`${backend} concurrent publications never overwrite the captured repository`, async () => {
    const { env, records } = fixture(backend);
    const snapshots = await Promise.all(Array.from({ length: 8 }, (_, index) => capture({ ...env, PUBLIC: { index } })));
    assert.equal(new Set(snapshots.map((snapshot) => snapshot.clone_url)).size, 8);
    assert.equal(records.size, 8);
    const heads = await Promise.all(snapshots.map(async (snapshot) => {
      const response = await createWorker(module).fetch(new Request(`${snapshot.clone_url}/info/refs`), env);
      assert.equal(response.status, 200);
      return response.text();
    }));
    assert.equal(new Set(heads).size, 8);
  });

  test(`real Git clones and fetches ${backend} snapshots over smart and dumb HTTP across isolates`, async () => {
    const { env, records } = fixture(backend);
    const server = createServer(async (incoming, outgoing) => {
      try {
        const body = [];
        for await (const chunk of incoming) body.push(chunk);
        const url = `http://127.0.0.1:${server.address().port}${incoming.url}`;
        const init = { method: incoming.method, headers: incoming.headers, ...(incoming.method === "POST" ? { body: Buffer.concat(body) } : {}) };
        // Deliberately reconstruct the host for every request, as isolate changes
        // must use persisted bytes rather than the current capture environment.
        const response = await createWorker(module).fetch(new Request(url, init), env);
        outgoing.writeHead(response.status, Object.fromEntries(response.headers));
        await pipeline(Readable.fromWeb(response.body), outgoing);
      } catch (error) {
        if (outgoing.headersSent) outgoing.destroy(error);
        else { outgoing.writeHead(500); outgoing.end(String(error)); }
      }
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const directory = await mkdtemp(join(tmpdir(), "git-zero-storage-"));
    const gitEnv = {
      ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("GIT_"))),
      GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0",
    };
    try {
      const snapshot = await capture(env, `http://127.0.0.1:${server.address().port}`);
      const captured = JSON.parse(new TextDecoder().decode([...records.values()][0])).parts;
      env.PUBLIC = "later environment";
      const heads = await Promise.all([false, true].map(async (dumb) => {
        const git = (...args) => execute("git", ["-c", "protocol.version=2", ...args], { env: { ...gitEnv, ...(dumb ? { GIT_SMART_HTTP: "0" } : {}) }, timeout: 30000 });
        const clone = join(directory, dumb ? "dumb" : "smart");
        await git("clone", "-q", snapshot.clone_url, clone);
        await git("-C", clone, "fsck", "--strict");
        for (const [index, file] of ["cf.json", "headers.json", "vars.json", "runtime.json"].entries()) {
          assert.equal(await readFile(join(clone, file), "utf8"), captured[index]);
        }
        await git("-C", clone, "fetch", "origin");
        return (await git("-C", clone, "rev-parse", "HEAD")).stdout.trim();
      }));
      assert.equal(heads[0], heads[1]);
    } finally {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
      await rm(directory, { recursive: true, force: true });
    }
  });
}

test("unknown snapshot storage settings fail without silently publishing inline data", async () => {
  const response = await createWorker(module).fetch(request("https://demo.example"), { SNAPSHOT_STORAGE: "other" });
  assert.equal(response.status, 503);
});

test("KV's physical TTL deletion reports a missing/expired snapshot without recapturing", async () => {
  const { env, records, state } = fixture("kv");
  const snapshot = await capture(env);
  state.offsetMs = 86400001;
  const response = await createWorker(module).fetch(new Request(`${snapshot.clone_url}/HEAD`), env);
  assert.equal(records.size, 0);
  assert.equal(response.status, 404);
  assert.match(await response.text(), /expired/);
  assert.equal(state.writes, 1);
});
