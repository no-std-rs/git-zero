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
const layouts = [
  { name: "UUID/text", schema: legacy, compact: false, binary: false },
  { name: "numeric/text", schema: legacy.replace("stage TEXT", "stage INTEGER"), compact: true, binary: false },
  { name: "numeric/binary", schema: null, compact: true, binary: true },
];
const blobs = { async delete() {} };
function store(database) { return new Store(sqliteContext(database), { BLOBS: blobs }, "catalog"); }

test("all graph layouts retain both object formats across reopen and cleanup", async () => {
  for (const format of [0, 1]) for (const layout of layouts) {
    const database = new DatabaseSync(":memory:");
    try {
      if (layout.schema) database.exec(layout.schema);
      let catalog = store(database);
      const first = catalog.stage(format), second = catalog.stage(format);
      const owner = catalog.edgeStage(first), other = catalog.edgeStage(second), oid = "1".repeat(format ? 64 : 40), target = catalog.edgeTarget(oid);
      assert.equal(typeof owner, layout.compact ? "number" : "string");
      assert.equal(catalog.binaryEdges, layout.binary);
      catalog.exec("INSERT INTO stage_edges VALUES(?,12,?,1,1)", owner, target);
      catalog.exec("INSERT INTO stage_edges VALUES(?,12,?,1,1)", other, target);
      catalog = store(database);
      assert.equal(catalog.edgeStage(first), owner);
      assert.equal(catalog.exec(`SELECT ${catalog.edgeTargetSQL()} AS dst FROM stage_edges WHERE stage=? AND node=12`, owner)[0].dst, oid);
      assert.equal(catalog.exec("SELECT typeof(dst) AS type FROM stage_edges LIMIT 1")[0].type, layout.binary ? "blob" : "text");
      assert.throws(() => catalog.exec("INSERT OR IGNORE INTO stage_edges VALUES(?,12,?,2,1)", owner, target), /conflicting graph target kinds/);
      if (layout.compact) assert.throws(() => catalog.exec("INSERT OR IGNORE INTO stage_edges VALUES(?,13,?,1,1)", first, target), /graph stage ID is invalid/);
      if (layout.binary) for (const invalid of [oid, new Uint8Array(19).buffer]) assert.throws(() => catalog.exec("INSERT INTO stage_edges VALUES(?,13,?,1,1)", owner, invalid), /graph object ID is invalid/);
      await catalog.abandon(first);
      assert.deepEqual(catalog.exec(`SELECT stage,${catalog.edgeTargetSQL()} AS dst FROM stage_edges`).map((row) => ({ ...row })), [{ stage: other, dst: oid }]);
      assert.equal(catalog.statistics().quarantineRows, 0);
      await catalog.abandon(second);
      assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM stage_edges")[0].count, 0);
      if (layout.compact) assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM edge_stages")[0].count, 0);
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

test("binary object IDs reduce both 100,000-edge catalogs while preserving indexed reads", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-edge-catalog-"));
  try {
    for (const format of ["sha1", "sha256"]) {
      const sizes = [];
      const ids = Array.from({ length: 10000 }, (_, index) => createHash(format).update(String(index)).digest("hex"));
      for (const [index, layout] of layouts.entries()) {
        const path = join(directory, `${format}-${index}.sqlite`), database = new DatabaseSync(path);
        try {
          if (layout.schema) database.exec(layout.schema);
          const catalog = store(database), stage = catalog.stage(format === "sha256" ? 1 : 0), owner = catalog.edgeStage(stage), targets = ids.map((id) => catalog.edgeTarget(id));
          catalog.ctx.storage.transactionSync(() => {
            for (let index = 0; index < 100000; index += 16) {
              const values = [];
              for (let at = index; at < index + 16; at++) values.push(owner, 2 ** 32 + 4096 * Math.floor(at / 100), targets[at % targets.length], 3, 3);
              catalog.exec("INSERT INTO stage_edges VALUES" + Array(16).fill("(?,?,?,?,?)").join(","), ...values);
            }
          });
          assert.equal(catalog.exec("SELECT COUNT(*) AS count FROM stage_edges")[0].count, 100000);
          const read = `SELECT ${catalog.edgeTargetSQL()} AS dst FROM stage_edges WHERE stage=? AND node=? AND relation=3`;
          assert.deepEqual(catalog.exec(read, owner, 2 ** 32).map((row) => row.dst), ids.slice(0, 100).sort());
          const plan = catalog.exec("EXPLAIN QUERY PLAN " + read, owner, 2 ** 32);
          assert.ok(plan.some((row) => /SEARCH stage_edges USING PRIMARY KEY/.test(row.detail)));
        } finally { database.close(); }
        sizes.push((await stat(path)).size);
      }
      assert.ok(sizes[1] < sizes[0] * 0.8, `${format} numeric stage keys: ${sizes}`);
      assert.ok(sizes[2] < sizes[1] * 0.8, `${format} binary object IDs: ${sizes}`);
      t.diagnostic(`${format}, 100,000 graph edges, UUID/text -> numeric/text -> numeric/binary: ${sizes.join(" -> ")} SQLite bytes`);
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});
