import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { Codec } from "../src/codec.mjs";
import { FetchJob } from "../src/fetch.mjs";
import { Store } from "../src/storage.mjs";
import { entry, object, pack, packet, project, receiveReport } from "./fixture.mjs";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { headers } from "./wrangler.mjs";

const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
function host(format) {
  const database = new DatabaseSync(":memory:"), objects = new Map();
  const BLOBS = {
    async put(key, data) { objects.set(key, new Uint8Array(data).slice()); },
    async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; },
    async delete(keys) { for (const key of Array.isArray(keys) ? keys : [keys]) objects.delete(key); },
  };
  const repository = new OwnRepository(sqliteContext(database), { HOST_TOKEN: headers().authorization.slice(7), BLOBS, REPO_FORMAT: String(format) });
  return { database, repository };
}
async function push(repository, format, packed, root, names) {
  const capability = `report-status-v2 atomic side-band-64k object-format=${format ? "sha256" : "sha1"}`;
  const commands = names.map((name, index) => packet(Buffer.concat([
    Buffer.from(`${"0".repeat(root.length * 2)} ${root.toString("hex")} `), Buffer.from(name), Buffer.from(index ? "\n" : `\0${capability}\n`),
  ])));
  const response = await repository.fetch(new Request("http://fixture/review.git/git-receive-pack", {
    method: "POST", headers: { ...headers(), "x-git-zero-namespace": "review" }, body: Buffer.concat([...commands, Buffer.from("0000"), packed]),
  }));
  const body = await response.arrayBuffer(); await repository.queue;
  return { status: response.status, body };
}

test("duplicate binary ref commands are client errors and leave the repository usable", async () => {
  for (const format of [0, 1]) {
    const { database, repository } = host(format), fixture = project(format);
    const name = Buffer.concat([Buffer.from("refs/heads/raw-"), Buffer.from([255])]);
    try {
      const rejected = await push(repository, format, fixture.pack, fixture.root.id, [name, name]);
      assert.equal(rejected.status, 400);
      assert.equal(repository.store.statistics().refs, 0); assert.deepEqual(repository.store.statistics().stages, []);
      const accepted = await push(repository, format, fixture.pack, fixture.root.id, [name]);
      assert.equal(accepted.status, 200); assert.match(receiveReport(accepted.body), /ok refs\/heads\/raw-/);
      assert.equal(repository.store.statistics().refs, 1); assert.equal(repository.store.statistics().objects, 3);
    } finally { database.close(); }
  }
});

function history(format) {
  const value = object(format, 3, randomBytes(1024));
  const tree = object(format, 2, Buffer.concat([Buffer.from("100644 file\0"), value.id]));
  const root = object(format, 1, `tree ${tree.id.toString("hex")}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\ncommit\n`);
  return { root, objects: [root, tree, value], pack: pack(format, [entry(3, value.body), entry(2, tree.body), entry(1, root.body)]) };
}
test("a small wanted closure remains fetchable when the retained catalog exceeds fetch limits", async () => {
  for (const format of [0, 1]) {
    const { database, repository } = host(format), first = history(format), second = history(format);
    try {
      for (const [fixture, branch] of [[first, "main"], [second, "side"]]) {
        const result = await push(repository, format, fixture.pack, fixture.root.id, [`refs/heads/${branch}`]);
        assert.equal(result.status, 200); assert.match(receiveReport(result.body), /ok refs\/heads\//);
      }
      assert.equal(repository.store.statistics().objects, 6);
      for (const version of [0, 1, 2]) for (const limits of [{ maxObjects: 3 }, { maxPackBytes: 1800 }]) {
        const codec = new Codec(module, format, limits), fetch = new FetchJob(repository.store, codec, version);
        const capability = `object-format=${format ? "sha256" : "sha1"}`;
        const controls = [packet(`want ${first.root.id.toString("hex")}${version === 2 ? "" : ` ${capability}`}\n`), ...(version === 2 ? [] : [Buffer.from("0000")]), packet("done\n"), ...(version === 2 ? [Buffer.from("0000")] : [])];
        if (version === 2) controls.unshift(packet("command=fetch\n"), packet(capability + "\n"), Buffer.from("0001"));
        await fetch.decode(new Request("http://fixture/review.git/git-upload-pack", { method: "POST", body: Buffer.concat(controls) }));
        assert.equal(fetch.rawSources, undefined);
        const selected = repository.store.exec("SELECT oid FROM fetch_selected WHERE included=1 ORDER BY oid").map((row) => row.oid);
        assert.deepEqual(selected, first.objects.map((item) => item.id.toString("hex")).sort());
        const chunks = []; for await (const chunk of fetch.response()) chunks.push(chunk);
        const response = Buffer.concat(chunks), at = response.indexOf("PACK");
        assert.ok(at >= 0); assert.equal(response.readUInt32BE(at + 8), 3);
      }
    } finally { database.close(); }
  }
});

test("abandoned pack deletion batches preserve shared chunks and retry ownership after a partial failure", async () => {
  for (const failFirst of [false, true]) {
    const database = new DatabaseSync(":memory:"), ctx = sqliteContext(database), objects = new Set();
    let store, stage, calls = 0, fail = failFirst, alarms = 0;
    ctx.storage.setAlarm = async () => { alarms++; };
    const BLOBS = { async delete(input) {
      calls++; const keys = Array.isArray(input) ? input : [input];
      assert.ok(keys.length > 0 && keys.length <= 128); assert.ok(!keys.includes("shared"));
      for (const key of keys) assert.equal(store.exec("SELECT 1 FROM stage_chunks WHERE stage=? AND key=?", stage, key).length, 1);
      objects.delete(keys[0]);
      if (fail) { fail = false; throw new Error("injected partial deletion failure"); }
      for (const key of keys) objects.delete(key);
    } };
    try {
      store = new Store(ctx, { BLOBS }, "review"); store.initialize(0); stage = store.stage(0);
      const published = store.stage(0); store.exec("UPDATE stages SET phase='published' WHERE id=?", published);
      for (let seq = 0; seq < 257; seq++) {
        const key = seq === 5 ? "shared" : `orphan-${seq}`; objects.add(key);
        store.exec("INSERT INTO stage_chunks VALUES(?,?,?,?,?,?)", stage, seq, key, seq, 1, "digest");
      }
      store.exec("INSERT INTO stage_chunks VALUES(?,0,'shared',0,1,'digest')", published);
      await store.abandon(stage, { defer: true });
      if (failFirst) {
        assert.equal(alarms, 1); assert.equal(store.exec("SELECT COUNT(*) AS count FROM stage_chunks WHERE stage=?", stage)[0].count, 257);
        await store.recover();
      }
      assert.equal(calls, failFirst ? 4 : 3);
      assert.deepEqual([...objects], ["shared"]);
      assert.deepEqual(store.exec("SELECT stage,key FROM stage_chunks").map((row) => [row.stage, row.key]), [[published, "shared"]]);
      assert.equal(store.exec("SELECT 1 FROM stages WHERE id=?", stage).length, 0);
    } finally { database.close(); }
  }
});
