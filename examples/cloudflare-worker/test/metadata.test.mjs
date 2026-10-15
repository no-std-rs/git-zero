import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { test } from "node:test";
import { createWorker } from "../src/worker.mjs";
import { withServer } from "./http.mjs";

const execute = promisify(execFile);
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
const worker = createWorker(module);
const origin = "https://demo.example";
const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
const tokenUrl = (parts, base = origin) => `${base}/r/${Buffer.from(JSON.stringify(parts)).toString("base64url")}/repo.git`;
const data = (url) => JSON.parse(Buffer.from(new URL(url).pathname.split("/")[2], "base64url").toString());
const env = { EXPOSED_VARS: "PUBLIC", PUBLIC: { enabled: true, values: [1, null, "text"] }, SECRET_TOKEN: "unselected-binding-secret" };

function request(secret = "access-secret", base = origin) {
  const value = new Request(`${base}/`, { headers: {
    "cf-ray": "public-CDG", "cf-ipcountry": "FR", "cf-access-jwt-assertion": secret,
    "cf-access-client-secret": secret, "cf-access-client-id": "private-client",
    "cf-access-authenticated-user-email": "private@example.test", authorization: `Bearer ${secret}`,
    cookie: `CF_Authorization=${secret}`,
  } });
  Object.defineProperty(value, "cf", { value: { colo: "CDG" } });
  return value;
}

async function capture(config = env, incoming = request()) {
  const response = await worker.fetch(incoming, config);
  assert.equal(response.status, 200);
  return (await response.json()).clone_url;
}

test("new captures select public headers and never include Access credentials in the URL", async () => {
  const first = await capture(env, request("first-secret"));
  assert.equal(first, await capture(env, request("second-secret")));
  const parts = data(first);
  assert.deepEqual(JSON.parse(parts[1]), { "cf-ray": "public-CDG" });
  assert.deepEqual(JSON.parse(parts[2]), { PUBLIC: env.PUBLIC });
  assert.ok(!JSON.stringify(parts).includes("secret"));
  assert.ok(!JSON.stringify(parts).includes("private"));
  // This is the pre-change flat encoding for ordinary public data. Keeping it
  // byte-identical also preserves its existing snapshot URL and Git objects.
  assert.equal(first, tokenUrl([
    json({ colo: "CDG" }), json({ "cf-ray": "public-CDG" }), json({ PUBLIC: env.PUBLIC }),
    json({ platform: "Cloudflare Workers", format: 1, history: "one root commit; no parents" }),
  ]));
});

test("header selection is explicit and refuses authentication header names", async () => {
  const selected = data(await capture({ ...env, EXPOSED_CF_HEADERS: "CF-Ray,cf-ipcountry" }));
  assert.deepEqual(JSON.parse(selected[1]), { "cf-ray": "public-CDG", "cf-ipcountry": "FR" });
  assert.deepEqual(JSON.parse(data(await capture({ ...env, EXPOSED_CF_HEADERS: "" }))[1]), {});
  for (const name of ["cf-access-jwt-assertion", "Cf-Access-Client-Secret", "cf-access-token", "cf-authorization", "authorization", "cookie"]) {
    const response = await worker.fetch(request(), { ...env, EXPOSED_CF_HEADERS: name });
    assert.equal(response.status, 503, name);
    assert.equal(response.headers.get("cache-control"), "no-store");
  }
});

test("selected bindings must be plain JSON without getter or toJSON execution", async () => {
  let invoked = 0;
  const accessor = Object.defineProperty({}, "secret", { enumerable: true, get() { invoked++; return "private"; } });
  const custom = { toJSON() { invoked++; return "private"; } };
  const cycle = {}; cycle.self = cycle;
  const invalid = [accessor, custom, cycle, new Date(0), new Uint8Array([1]), () => "private", 1n, NaN, Infinity, [undefined], Array(2)];
  for (const PUBLIC of invalid) {
    const response = await worker.fetch(request(), { EXPOSED_VARS: "PUBLIC", PUBLIC });
    assert.equal(response.status, 503);
    assert.ok(!(await response.text()).includes("private"));
  }
  assert.equal(invoked, 0);
  // Opaque bindings remain untouched when absent from the explicit public list.
  const config = { ...env };
  Object.defineProperty(config, "UNSELECTED", { enumerable: true, get() { invoked++; throw new Error("private"); } });
  await capture(config);
  assert.equal(invoked, 0);
  const selected = { EXPOSED_VARS: "PUBLIC" };
  Object.defineProperty(selected, "PUBLIC", { get() { invoked++; return "private"; } });
  assert.equal((await worker.fetch(request(), selected)).status, 503);
  assert.equal(invoked, 0);
});

test("plain JSON aliases and special object keys preserve their actual public values", async () => {
  const shared = { value: "public" };
  const PUBLIC = { first: shared, second: shared, ...JSON.parse('{"__proto__":"data"}') };
  const parts = data(await capture({ EXPOSED_VARS: "PUBLIC", PUBLIC }));
  assert.deepEqual(JSON.parse(parts[2]), { PUBLIC });
  const config = Object.assign(Object.create(null), { EXPOSED_VARS: "__proto__,missing", __proto__: "not-an-own-property" });
  Object.defineProperty(config, "__proto__", { value: "public-prototype-name", enumerable: true });
  assert.deepEqual(JSON.parse(data(await capture(config))[2]), JSON.parse('{"__proto__":"public-prototype-name"}'));
});

test("projection limits fail before any persisted snapshot is published", async () => {
  let writes = 0;
  const config = { ...env, SNAPSHOT_STORAGE: "kv", SNAPSHOTS_KV: { get() {}, put() { writes++; } } };
  let deep = "leaf";
  for (let index = 0; index < 18; index++) deep = { child: deep };
  for (const PUBLIC of [deep, Array(1025).fill(0), "x".repeat(9000)]) {
    const response = await worker.fetch(request(), { ...config, PUBLIC });
    assert.equal(response.status, 413);
    assert.equal(response.headers.get("cache-control"), "no-store");
  }
  for (const EXPOSED_VARS of [Array(65).fill("PUBLIC").join(","), "x".repeat(4097), ["PUBLIC"]]) {
    assert.equal((await worker.fetch(request(), { ...config, EXPOSED_VARS })).status, 503);
  }
  assert.equal(writes, 0);
});

async function checkClone(directory, url, expectedHeaders, dumb) {
  const config = {
    ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("GIT_"))),
    GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0",
    ...(dumb ? { GIT_SMART_HTTP: "0" } : {}),
  };
  const git = (...args) => execute("git", ["-c", "protocol.version=2", ...args], { env: config, timeout: 30000 });
  const clone = join(directory, dumb ? "dumb" : "smart");
  await git("clone", "-q", url, clone);
  await git("-C", clone, "fsck", "--strict");
  assert.deepEqual(JSON.parse(await readFile(join(clone, "headers.json"), "utf8")), expectedHeaders);
  const ids = (await git("-C", clone, "rev-list", "--objects", "--all")).stdout.trim().split("\n").map((line) => line.split(" ")[0]);
  for (const id of ids) {
    const object = (await git("-C", clone, "cat-file", "-p", id)).stdout;
    assert.ok(!object.includes("access-secret") && !object.includes("unselected-binding-secret") && !object.includes("private@example.test"));
  }
  return (await git("-C", clone, "rev-parse", "HEAD")).stdout.trim();
}

test("real Git imports only selected public objects over smart and dumb HTTP", async () => {
  await withServer((incoming) => worker.fetch(incoming, env), async (base) => {
    const url = await capture(env, request("access-secret", base));
    const directory = await mkdtemp(join(tmpdir(), "git-zero-public-"));
    try {
      const heads = await Promise.all([false, true].map((dumb) => checkClone(directory, url, { "cf-ray": "public-CDG" }, dumb)));
      assert.equal(heads[0], heads[1]);
    } finally { await rm(directory, { recursive: true, force: true }); }
  });
});

test("existing flat URLs keep their original objects regardless of new capture selection", async () => {
  const parts = [json({ colo: "old" }), json({ "cf-custom": "legacy-public-data" }), json({ old: true }), json({ format: 1 })];
  const url = tokenUrl(parts);
  const first = await worker.fetch(new Request(`${url}/info/refs?service=git-upload-pack`), { EXPOSED_CF_HEADERS: "cookie" });
  const second = await createWorker(module).fetch(new Request(`${url}/info/refs?service=git-upload-pack`), {});
  assert.equal(first.status, 200);
  assert.equal(await first.text(), await second.text());
});
