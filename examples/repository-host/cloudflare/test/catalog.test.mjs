import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { Store } from "../src/storage.mjs";
import { sqliteContext } from "./sqlite.mjs";

const legacy = "CREATE TABLE stage_edges(stage TEXT NOT NULL,node INTEGER NOT NULL,dst TEXT NOT NULL,kind INTEGER NOT NULL,relation INTEGER NOT NULL,PRIMARY KEY(stage,node,relation,dst)) WITHOUT ROWID";
const blobs = { async delete() {} };
function store(database) { return new Store(sqliteContext(database), { BLOBS: blobs }, "catalog"); }

test("numeric graph keys and existing text catalogs retain graph semantics across reopen and cleanup", async () => {
  for (const old of [false, true]) {
    const database = new DatabaseSync(":memory:");
    try {
      if (old) database.exec(legacy);
      let catalog = store(database);
      const first = catalog.stage(0), second = catalog.stage(0);
      const owner = catalog.edgeStage(first), other = catalog.edgeStage(second), oid = "1".repeat(40);
      assert.equal(typeof owner, old ? "string" : "number");
      catalog.exec("INSERT INTO stage_edges VALUES(?,12,?,1,1)", owner, oid);
      catalog.exec("INSERT INTO stage_edges VALUES(?,12,?,1,1)", other, oid);
      catalog = store(database);
      assert.equal(catalog.edgeStage(first), owner);
      assert.equal(catalog.exec("SELECT dst FROM stage_edges WHERE stage=? AND node=12", owner)[0].dst, oid);
      assert.throws(() => catalog.exec("INSERT OR IGNORE INTO stage_edges VALUES(?,12,?,2,1)", owner, oid), /conflicting graph target kinds/);
      if (!old) assert.throws(() => catalog.exec("INSERT OR IGNORE INTO stage_edges VALUES(?,13,?,1,1)", first, oid), /graph stage ID is invalid/);
      await catalog.abandon(first);
      assert.deepEqual(catalog.exec("SELECT stage,dst FROM stage_edges").map((row) => ({ ...row })), [{ stage: other, dst: oid }]);
      assert.equal(catalog.statistics().quarantineRows, 0);
      await catalog.abandon(second);
      assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM stage_edges")[0].count, 0);
      if (!old) assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM edge_stages")[0].count, 0);
    } finally { database.close(); }
  }
});

test("a failed numeric-key allocation rolls back its stage owner", () => {
  const database = new DatabaseSync(":memory:");
  try {
    const catalog = store(database), execute = catalog.exec.bind(catalog);
    catalog.exec = (sql, ...bindings) => {
      if (sql.startsWith("INSERT INTO edge_stages")) throw new Error("injected allocation failure");
      return execute(sql, ...bindings);
    };
    assert.throws(() => catalog.stage(0), /injected allocation failure/);
    catalog.exec = execute;
    assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM stages")[0].count, 0);
    assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM edge_stages")[0].count, 0);
  } finally { database.close(); }
});

test("interned graph handles reduce the 100,000-edge catalog without replacing indexed reads", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-edge-catalog-"));
  try {
    const sizes = [];
    const ids = Array.from({ length: 10000 }, (_, index) => createHash("sha1").update(String(index)).digest("hex"));
    for (const old of [true, false]) {
      const path = join(directory, `${old ? "text" : "integer"}.sqlite`), database = new DatabaseSync(path);
      try {
        if (old) database.exec(legacy);
        const catalog = store(database), stage = catalog.stage(0), owner = catalog.edgeStage(stage);
        catalog.ctx.storage.transactionSync(() => {
          for (let index = 0; index < 100000; index += 16) {
            const values = [];
            for (let at = index; at < index + 16; at++) values.push(owner, 2 ** 32 + 4096 * Math.floor(at / 100), ids[at % ids.length], 3, 3);
            catalog.exec("INSERT INTO stage_edges VALUES" + Array(16).fill("(?,?,?,?,?)").join(","), ...values);
          }
        });
        assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM stage_edges")[0].count, 100000);
        assert.equal(catalog.exec("SELECT dst FROM stage_edges WHERE stage=? AND node=? AND relation=3", owner, 2 ** 32).length, 100);
        const plan = catalog.exec("EXPLAIN QUERY PLAN SELECT dst FROM stage_edges WHERE stage=? AND node=? AND relation=3", owner, 2 ** 32);
        assert.ok(plan.some((row) => /SEARCH stage_edges USING PRIMARY KEY/.test(row.detail)));
      } finally { database.close(); }
      sizes.push((await stat(path)).size);
    }
    assert.ok(sizes[1] < sizes[0] * 0.8, `${sizes[0]} -> ${sizes[1]} bytes`);
    t.diagnostic(`100,000 graph edges: ${sizes[0]} -> ${sizes[1]} SQLite bytes`);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
