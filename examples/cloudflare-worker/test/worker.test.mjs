import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { test } from "node:test";
import { createWorker } from "../src/worker.mjs";

const execute = promisify(execFile);
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
const worker = createWorker(module);
const env = { EXPOSED_VARS: "DEMO_NAME,PUBLIC_JSON", DEMO_NAME: "test", PUBLIC_JSON: { public: true }, SECRET_TOKEN: "never-export-this" };

function request(url, init) {
  const result = new Request(url, init);
  Object.defineProperty(result, "cf", { value: { asn: 13335, colo: "CDG", country: "FR", tlsVersion: "TLSv1.3" } });
  return result;
}

async function snapshot(origin = "https://demo.example") {
  const response = await worker.fetch(request(origin, { headers: { "cf-ray": "example-CDG", authorization: "private" } }), env);
  assert.equal(response.status, 200);
  return (await response.json()).clone_url;
}

async function withServer(callback) {
  const server = createServer(async (incoming, outgoing) => {
    try {
      const parts = [];
      for await (const part of incoming) parts.push(part);
      const url = `http://127.0.0.1:${server.address().port}${incoming.url}`;
      const init = { method: incoming.method, headers: incoming.headers };
      if (incoming.method === "POST") init.body = Buffer.concat(parts);
      const response = await worker.fetch(request(url, init), env);
      outgoing.writeHead(response.status, Object.fromEntries(response.headers));
      outgoing.end(Buffer.from(await response.arrayBuffer()));
    } catch (error) {
      outgoing.writeHead(500);
      outgoing.end(String(error));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    await callback(`http://127.0.0.1:${server.address().port}`);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}

test("Wasm uses no host imports or allocator", () => {
  assert.deepEqual(WebAssembly.Module.imports(module), []);
  const exports = WebAssembly.Module.exports(module).map((item) => item.name);
  assert.ok(exports.includes("serve"));
  assert.ok(!exports.some((name) => /malloc|alloc|realloc|free/.test(name)));
});

test("metadata snapshot exposes available public data and pins objects across requests", async () => {
  const url = await snapshot();
  const token = new URL(url).pathname.split("/")[2];
  const parts = JSON.parse(Buffer.from(token, "base64url").toString("utf8"));
  assert.equal(JSON.parse(parts[0]).colo, "CDG");
  assert.equal(JSON.parse(parts[1])["cf-ray"], "example-CDG");
  assert.equal(JSON.parse(parts[2]).DEMO_NAME, "test");
  assert.ok(!JSON.stringify(parts).includes("never-export-this"));
  assert.ok(!JSON.stringify(parts).includes("private"));
  const responses = await Promise.all(Array.from({ length: 16 }, () => worker.fetch(new Request(`${url}/info/refs?service=git-upload-pack`), {})));
  const advertisements = await Promise.all(responses.map((response) => response.text()));
  assert.equal(new Set(advertisements).size, 1);
});

test("invalid snapshots, push and oversized metadata fail predictably", async () => {
  assert.equal((await worker.fetch(new Request("https://demo.example/r/xxx/repo.git/HEAD"))).status, 400);
  assert.equal((await worker.fetch(new Request("https://demo.example/r/unknown/repo.git/HEAD", { method: "DELETE" }))).status, 405);
  const large = new Request("https://demo.example/");
  Object.defineProperty(large, "cf", { value: { large: "x".repeat(9000) } });
  assert.equal((await worker.fetch(large)).status, 413);
  const url = await snapshot();
  assert.equal((await worker.fetch(new Request(`${url}/info/refs?service=git-receive-pack`))).status, 403);
});

for (const dumb of [false, true]) {
  test(`real Git clones the Wasm Worker over ${dumb ? "dumb" : "smart"} HTTP`, { concurrency: true }, async () => {
    await withServer(async (origin) => {
      const directory = await mkdtemp(join(tmpdir(), "git-zero-wasm-"));
      const gitEnv = {
        ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_"))),
        GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0",
        ...(dumb ? { GIT_SMART_HTTP: "0" } : {}),
      };
      const git = (...args) => execute("git", args, { env: gitEnv, timeout: 30000 });
      try {
        const cloneUrl = await snapshot(origin);
        // Independent real clients overlap against the same Worker handler and
        // snapshot, exercising isolate buffer ownership during Git POST bodies.
        await Promise.all(Array.from({ length: 3 }, async (_, index) => {
          const clone = join(directory, `clone-${index}`);
          await git("clone", "-q", cloneUrl, clone);
          await git("-C", clone, "fsck", "--strict");
          const history = await git("-C", clone, "rev-list", "--count", "HEAD");
          assert.equal(history.stdout.trim(), "1");
          assert.equal(JSON.parse(await readFile(join(clone, "cf.json"), "utf8")).colo, "CDG");
          assert.deepEqual(JSON.parse(await readFile(join(clone, "vars.json"), "utf8")), { DEMO_NAME: "test", PUBLIC_JSON: { public: true } });
          await git("-C", clone, "fetch", "origin");
        }));
      } finally {
        await rm(directory, { recursive: true, force: true });
      }
    });
  });
}
