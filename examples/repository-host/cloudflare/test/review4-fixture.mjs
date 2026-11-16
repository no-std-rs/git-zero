import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { createRepositoryHost } from "../src/host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { project, receiveReport, packet } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
const options = { codec, bindings: { repositories: "CUSTOM_REPOS", objects: "SHARED_DATA", grantKey: "PRODUCT_KEY" }, authMode: "gateway", routePrefix: "/private/git", keyPrefix: "product/git/" };
export async function holdFetch(f, name) {
  const data = project(0), tip = data.root.id.toString("hex");
  const exists = f.repo(name).store?.exec("SELECT 1 FROM refs LIMIT 1").length;
  const pushed = !exists && await f.repo(name).fetch(f.request(name, "git-receive-pack", "write", { method: "POST", headers: { "content-type": "application/x-git-receive-pack-request" }, body: receive(0, [{ name: "refs/heads/main", old: "0".repeat(40), new: tip }], data.pack) }));
  if (pushed) assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
  return f.repo(name).fetch(f.request(name, "git-upload-pack", "read", { method: "POST", headers: { "content-type": "application/x-git-upload-pack-request" }, body: Buffer.concat([packet(`want ${tip} side-band-64k\n`), Buffer.from("0000"), packet("done\n")]) }));
}
export function fixture(extra = {}) {
  const host = createRepositoryHost({ ...options, ...extra }), objects = new Map(), databases = [], instances = new Map();
  const env = { PRODUCT_KEY: GRANT_KEY, SHARED_DATA: { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, value) { objects.set(key, new Uint8Array(value).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); }, async list({ prefix, limit }) { const keys = [...objects.keys()].filter((key) => key.startsWith(prefix)).sort(); return { objects: keys.slice(0, limit).map((key) => ({ key })), truncated: keys.length > limit }; } } };
  env.CUSTOM_REPOS = { idFromName: (name) => ({ toString: () => name }), get(id) { const name = id.toString(); if (!instances.has(name)) { const db = new DatabaseSync(":memory:"); databases.push(db); instances.set(name, new host.Repository({ ...sqliteContext(db), id }, env)); } return instances.get(name); } };
  const request = (ns, route, level = "read", init = {}, expiry) => new Request(`http://host/private/git/${ns}.git/${route}`, { ...init, headers: { "x-git-zero-namespace": ns, "x-git-zero-grant": mint(ns, level, expiry ? { expiry } : {}), ...(env.TOKEN ? { authorization: `Bearer ${env.TOKEN}` } : {}), ...init.headers } });
  return { host, env, objects, instances, request, repo: (ns) => env.CUSTOM_REPOS.get(env.CUSTOM_REPOS.idFromName(ns)), close: () => databases.forEach((db) => db.close()) };
}
