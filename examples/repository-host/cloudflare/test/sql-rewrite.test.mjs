import test from "node:test";
import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import { jobStore } from "../src/jobs.mjs";
import { FETCH_TABLES } from "../src/storage.mjs";

test("job SQL templates compile once; report cached and former per-statement rewriting cost", (t) => {
  const owner = { id: crypto.randomUUID(), abort: new AbortController(), snapshot: { generation: 7, epoch: "public-test-epoch", proof_floor: 0, head: new TextEncoder().encode("refs/heads/main").buffer, object_count: 150000 } };
  let last;
  const base = { env: {}, exec(sql) { last = sql; return []; } }, scoped = jobStore(base, owner, { fetch: true, read: true });
  const template = "SELECT f.oid,o.kind FROM fetch_selected f JOIN stage_objects o ON o.stage=f.stage AND o.node=f.node JOIN stages s ON s.id=o.stage WHERE included=1 AND f.oid>? LIMIT 128";
  const targets = FETCH_TABLES.map((name) => [name, `job_${owner.id.replaceAll("-", "")}_${name}`]);
  const previous = (sql) => {
    for (const [name, target] of targets) sql = sql.replaceAll(new RegExp(`\\b${name}\\b`, "g"), target);
    sql = sql.replaceAll(/\brefs\b/g, "(SELECT v.name,v.oid,v.peeled FROM ref_names n JOIN ref_versions v ON v.name=n.name AND v.first_generation=(SELECT MAX(w.first_generation) FROM ref_versions w WHERE w.name=n.name AND w.first_generation<=7) WHERE v.last_generation IS NULL OR v.last_generation>7)");
    return sql.replaceAll(/\brepository\b/g, "(SELECT * FROM repository WHERE id=1)").replaceAll(/\brepository_security\b/g, "(SELECT * FROM repository_security WHERE id=1)").replaceAll(/\bstages\b/g, "(SELECT * FROM stages WHERE phase<>'published' OR published_generation<=7)");
  };
  const cached = [], former = [], count = 50000;
  for (let sample = 0; sample < 3; sample++) {
    let start = performance.now(); for (let i = 0; i < count; i++) previous(template); former.push((performance.now() - start) * 1000 / count);
    start = performance.now(); for (let i = 0; i < count; i++) scoped.exec(template, "a"); cached.push((performance.now() - start) * 1000 / count);
  }
  assert.equal(scoped.sqlMetrics.compilations, 1); assert.equal(scoped.sqlMetrics.hits, 3 * count - 1);
  assert.match(last, /published_generation<=7/); assert.match(last, /job_[0-9a-f]+_fetch_selected/);
  t.diagnostic(JSON.stringify({ microsecondsPerStatement: { former, cached }, statementsPerSample: count, runtime: process.version }));
});
