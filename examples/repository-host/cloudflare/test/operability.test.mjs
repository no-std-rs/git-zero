import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { createRepositoryHost } from "../src/host.mjs";
import { Store } from "../src/storage.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const options = { codec, bindings: { repositories: "REPOS", objects: "DATA", grantKey: "KEY", token: "TOKEN" }, authMode: "gateway", routePrefix: "", keyPrefix: "" };
function fixture(extra = {}) {
  const host = createRepositoryHost({ ...options, ...extra }), objects = new Map(), instances = new Map(), contexts = new Map(), scopes = [];
  const env = { KEY: GRANT_KEY, TOKEN: "public-fixture-token", DATA: {
    async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; },
    async put(key, value) { objects.set(key, new Uint8Array(value).slice()); },
    async delete(keys) { for (const key of keys) objects.delete(key); },
    async list({ prefix, limit }) { const keys = [...objects.keys()].filter((key) => key.startsWith(prefix)); return { objects: keys.slice(0, limit).map((key) => ({ key })), truncated: keys.length > limit }; },
  } };
  function binding(scope = "global") { return { jurisdiction(value) { scopes.push(value); return binding(value); }, idFromName(name) { return { toString: () => `${scope}:${name}` }; }, get(id) { const key = id.toString(); if (!instances.has(key)) { const db = new DatabaseSync(":memory:"), ctx = { ...sqliteContext(db), id }; contexts.set(key, { db, ctx }); instances.set(key, new host.Repository(ctx, env)); } return instances.get(key); } }; }
  env.REPOS = binding();
  const repo = (name, scope = extra.jurisdiction ?? "global") => env.REPOS.get(binding(scope).idFromName(name));
  const request = (name, path, method = "GET", body) => new Request(`http://host/${name}.git/${path}`, { method, headers: { "x-git-zero-namespace": name, "x-git-zero-grant": mint(name, "admin"), authorization: "Bearer public-fixture-token", "content-type": "application/json" }, ...(body === undefined ? {} : { body }) });
  return { host, env, objects, contexts, scopes, repo, request, close() { for (const { db } of contexts.values()) db.close(); } };
}

test("alarms cannot assign a new prefix to legacy data before the request guard", async () => {
  const f = fixture();
  try {
    const repo = f.repo("legacy"); assert.equal((await repo.fetch(f.request("legacy", "api/v1/repository", "PUT", "{}"))).status, 201);
    const { db, ctx } = f.contexts.get("global:legacy"); db.exec("DROP TABLE host_storage_config");
    f.objects.set("repositories/legacy/chunks/old", Uint8Array.of(1));
    const changed = createRepositoryHost({ ...options, keyPrefix: "new/" });
    await assert.rejects(new changed.Repository(ctx, f.env).alarm(), /legacy repository requires the original empty/);
    assert.equal(db.prepare("SELECT 1 FROM sqlite_master WHERE name='host_storage_config'").get(), undefined);
    assert.ok(f.objects.has("repositories/legacy/chunks/old"));
    const original = new f.host.Repository(ctx, f.env);
    assert.equal((await original.fetch(f.request("legacy", "api/v1/repository", "DELETE"))).status, 202);
    assert.equal((await original.fetch(f.request("legacy", "api/v1/repository", "DELETE"))).status, 200);
    assert.equal(f.objects.has("repositories/legacy/chunks/old"), false);
  } finally { f.close(); }
});

test("EU namespace identity is applied to entry routing and the stub's own check", async () => {
  const f = fixture({ jurisdiction: "eu" });
  try {
    assert.equal((await f.host.fetch(f.request("eu-repo", "api/v1/repository", "PUT", "{}"), f.env)).status, 201);
    assert.ok(f.contexts.has("eu:eu-repo"));
    assert.equal((await f.repo("eu-repo").fetch(f.request("eu-repo", "api/v1/refs"))).status, 200);
    assert.equal((await f.repo("eu-repo", "global").fetch(f.request("eu-repo", "api/v1/refs"))).status, 404);
    assert.equal((await f.repo("eu-repo").fetch(f.request("another", "api/v1/refs"))).status, 404);
    assert.ok(f.scopes.length >= 3 && f.scopes.every((scope) => scope === "eu"));
    assert.throws(() => createRepositoryHost({ ...options, jurisdiction: "unknown" }), /jurisdiction/);
  } finally { f.close(); }
});

for (const value of ["false", "garbage", "0", 1, null, undefined]) test(`environment boolean ${JSON.stringify(value)} cannot enable diagnostics or implicit creation`, async () => {
  const f = fixture({ authMode: "token", implicitCreate: value, testPrefix: "/debug", testEnabled: value });
  try {
    const denied = await f.host.fetch(new Request("http://host/debug/repo/state", { headers: { authorization: "Bearer public-fixture-token" } }), f.env); assert.equal(denied.status, 404);
    const missing = await f.repo("repo").fetch(f.request("repo", "info/refs?service=git-upload-pack")); assert.equal(missing.status, 404);
    const { db } = f.contexts.get("global:repo"); assert.equal(db.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table'").get().n, 0);
  } finally { f.close(); }
});

test("uncreated authorized namespaces and invalid creates leave no schema or identity", async () => {
  const f = fixture();
  try {
    for (const path of ["api/v1/refs", "api/v1/repository", "info/refs?service=git-upload-pack", "unknown"]) {
      const name = `missing-${f.contexts.size}`, response = await f.repo(name).fetch(f.request(name, path)); assert.equal(response.status, 404);
    }
    assert.equal((await f.repo("bad").fetch(f.request("bad", "api/v1/repository", "PUT", '{"defaultBranch":"bad..name"}'))).status, 400);
    for (const { db, ctx } of f.contexts.values()) { assert.equal(db.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table'").get().n, 0); assert.equal(await ctx.storage.get("host.deletion.v1"), undefined); }
    assert.equal(f.objects.size, 0);
    assert.equal((await f.repo("new").fetch(f.request("new", "api/v1/repository", "DELETE"))).status, 404);
    assert.equal((await f.repo("new").fetch(f.request("new", "api/v1/repository", "PUT", "{}"))).status, 201);
  } finally { f.close(); }
});

test("older schema-only identities are purgeable even without a repository row", async () => {
  const f = fixture();
  try {
    const repo = f.repo("ghost"), { ctx, db } = f.contexts.get("global:ghost"); new Store(ctx, repo.env, "ghost");
    f.objects.set("repositories/ghost/tmp/orphan", Uint8Array.of(1));
    assert.equal((await repo.fetch(f.request("ghost", "api/v1/repository", "DELETE"))).status, 202);
    assert.equal((await repo.fetch(f.request("ghost", "api/v1/repository", "DELETE"))).status, 200);
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table'").get().n, 0);
    assert.equal(f.objects.has("repositories/ghost/tmp/orphan"), false);
  } finally { f.close(); }
});
