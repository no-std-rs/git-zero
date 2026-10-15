import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { treeRepository } from "../src/tree.mjs";
import { importRepository } from "../src/repository.mjs";
import { git as oracleGit, oracle } from "./tree-fixture.mjs";

const json = (value) => JSON.stringify(value, null, 2) + "\n";
const parts = (cf = {}, vars = {}) => [json(cf), json({}), json(vars), json({ platform: "Cloudflare Workers", format: 2 })];
const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));

test("tree metadata IDs and encoded paths match Git's independent object constructors", async () => {
  const value = [json({ colo: "CDG", tlsClientAuth: { certPresented: "0" } }), json({ "cf-ray": "public-CDG" }),
    json({ PUBLIC: [true, null], "%": 1, "/": 2, "..": 3, "café": 4, "": 5 }), json({ platform: "Cloudflare Workers", format: 2 })];
  const built = await treeRepository(value);
  const expected = new Map([
    ["cf/colo.json", json("CDG")], ["cf/tlsClientAuth.json", json({ certPresented: "0" })],
    ["headers/cf-ray.json", json("public-CDG")], ["vars/PUBLIC.json", json([true, null])],
    ["vars/%25.json", json(1)], ["vars/%2F.json", json(2)], ["vars/%2E%2E.json", json(3)],
    ["vars/caf%C3%A9.json", json(4)], ["vars/.json", json(5)],
    ["runtime/platform.json", json("Cloudflare Workers")], ["runtime/format.json", json(2)],
  ]);
  assert.deepEqual(built.files, [...expected.keys()].sort());
  const directory = await mkdtemp(join(tmpdir(), "git-zero-tree-oracle-"));
  const git = (args, input) => oracleGit(directory, args, input);
  try {
    const commit = oracle(directory, expected);
    assert.equal(built.repository.head.id, commit);
    const ids = git(["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]).toString().trim().split("\n").sort();
    assert.deepEqual(ids, built.repository.objects.map((value) => value.id).sort());
    for (const object of built.repository.objects) assert.deepEqual(git(["cat-file", object.kind, object.id]), Buffer.from(object.body));
    assert.deepEqual(await treeRepository(value), built);
    importRepository(new WebAssembly.Instance(module, {}).exports, built.repository);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("tree metadata omits empty categories and deduplicates equal bodies", async () => {
  const built = await treeRepository(parts({}, { A: 2, B: 2 }));
  assert.ok(built.files.every((file) => !file.startsWith("cf/") && !file.startsWith("headers/")));
  assert.equal(built.repository.objects.filter((object) => object.kind === "blob" && Buffer.from(object.body).toString() === json(2)).length, 1);
  assert.ok(built.repository.objects.filter((object) => object.kind === "tree").every((object) => object.body.length > 0));
  importRepository(new WebAssembly.Instance(module, {}).exports, built.repository);
});

test("tree metadata bounds files, encoded names, Unicode and serialized input", async () => {
  const fail = async (input, status) => assert.rejects(() => treeRepository(input), (error) => error.status === status);
  const exact = await treeRepository(parts({}, Object.fromEntries(Array.from({ length: 54 }, (_, index) => [`v${index}`, index]))));
  assert.equal(exact.files.length, 56);
  importRepository(new WebAssembly.Instance(module, {}).exports, exact.repository);
  await fail(parts({}, Object.fromEntries(Array.from({ length: 55 }, (_, index) => [`v${index}`, index]))), 413);
  await fail(parts({ ["a".repeat(251)]: true }), 413);
  const longest = await treeRepository(parts({ ["a".repeat(250)]: true }));
  assert.ok(longest.files.includes("cf/" + "a".repeat(250) + ".json"));
  importRepository(new WebAssembly.Instance(module, {}).exports, longest.repository);
  await fail(parts({ [String.fromCharCode(0xd800)]: true }), 400);
  await fail(parts({ padding: "p".repeat(8192) }), 413);
  await fail([json([]), json({}), json({}), json({})], 400);
  await fail(["invalid", json({}), json({}), json({})], 400);
  await fail([json({}), json({}), json({}), json({})], 400);
});
