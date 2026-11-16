import { ApiError } from "./api.mjs";
import { FETCH_TABLES } from "./storage.mjs";

// Admission bounds concurrency; it never holds a repository-wide execution lock.
export class JobGate {
  constructor(limit, pending, waitMs) { this.limit = limit; this.maximumPending = pending; this.waitMs = waitMs; this.active = 0; this.waiters = []; }
  enter(signal) {
    return new Promise((resolve, reject) => {
      if (this.active >= this.limit && this.waiters.length >= this.maximumPending) { reject(new ApiError("repository concurrency queue full; retry later", 503)); return; }
      if (signal?.aborted) { reject(new ApiError("repository job cancelled", 503)); return; }
      const item = { resolve, reject, signal };
      item.abort = () => { clearTimeout(item.timer); this.waiters = this.waiters.filter((entry) => entry !== item); reject(new ApiError("repository job cancelled", 503)); };
      signal?.addEventListener("abort", item.abort, { once: true });
      if (this.waitMs) item.timer = setTimeout(() => { this.waiters = this.waiters.filter((entry) => entry !== item); reject(new ApiError("repository concurrency wait expired; retry later", 503)); }, this.waitMs);
      this.waiters.push(item); this.pump();
    });
  }
  pump() {
    while (this.active < this.limit && this.waiters.length) {
      const item = this.waiters.shift(); clearTimeout(item.timer); item.signal?.removeEventListener("abort", item.abort); this.active++; let released = false;
      item.resolve(() => { if (released) return; released = true; this.active--; this.pump(); });
    }
  }
}

const literal = (value) => "'" + value.replaceAll("'", "''") + "'";
export function snapshot(store) {
  return store.exec("SELECT r.*,s.epoch,s.proof_floor FROM repository r JOIN repository_security s ON s.id=r.id WHERE r.id=1")[0];
}

// Private SQL tables make existing Rust-directed selection work queues truly
// request-local. No cursor/SQL statement remains borrowed across an await.
export function jobStore(base, owner, { read = false, fetch = false } = {}) {
  const view = Object.create(base), names = new Map(), state = read ? owner.snapshot : null;
  Object.defineProperty(view, "proofKey", { get: () => base.proofKey, set: (value) => { base.proofKey = value; } });
  Object.defineProperty(view, "lastIntakeBudget", { get: () => base.lastIntakeBudget, set: (value) => { base.lastIntakeBudget = value; } });
  Object.defineProperty(view, "recoveryDue", { get: () => base.recoveryDue, set: (value) => { base.recoveryDue = value; } });
  view.base = base; view.jobId = owner.id; view.snapshot = state;
  view.env = { ...base.env, JOB_SIGNAL: owner.abort.signal, JOB_OWNER: owner };
  if (fetch) for (const table of FETCH_TABLES) names.set(table, `job_${owner.id.replaceAll("-", "")}_${table}`);
  // Compile each SQL template once per snapshot/scratch assignment. In
  // particular, object walks must not run six regex rewrites per statement.
  const compiled = new Map();
  view.sqlMetrics = { compilations: 0, hits: 0 };
  const compile = (input) => {
    const cached = compiled.get(input);
    if (cached) { view.sqlMetrics.hits++; return cached; }
    let sql = input; const injected = [];
    for (const [name, target] of names) sql = sql.replaceAll(new RegExp(`\\b${name}\\b`, "g"), target);
    if (state && /^\s*(SELECT|WITH)\b/i.test(sql)) {
      // Publication closes disjoint validity intervals in the same transaction
      // as the ref CAS. No per-name MAX subquery is necessary.
      sql = sql.replaceAll(/\brefs\b/g, `(SELECT name,oid,peeled FROM ref_versions WHERE first_generation<=${state.generation} AND (last_generation IS NULL OR last_generation>${state.generation}))`);
      sql = sql.replaceAll(/\brepository\b/g, (match, offset, original) => {
        injected.push({ index: (original.slice(0, offset).match(/\?/g) ?? []).length, value: state.head });
        return `(SELECT id,format,${state.generation} AS generation,${state.object_count} AS object_count,? AS head FROM repository WHERE id=1)`;
      });
      sql = sql.replaceAll(/\brepository_security\b/g, `(SELECT id,${literal(state.epoch)} AS epoch,${state.proof_floor} AS proof_floor FROM repository_security WHERE id=1)`);
      sql = sql.replaceAll(/\bstages\b/g, `(SELECT * FROM stages WHERE phase<>'published' OR published_generation<=${state.generation})`);
    }
    if (compiled.size >= 256) compiled.delete(compiled.keys().next().value);
    const result = { sql, injected }; compiled.set(input, result); view.sqlMetrics.compilations++; return result;
  };
  view.exec = (sql, ...bindings) => {
    const query = compile(sql); let added = 0;
    for (const item of query.injected) bindings.splice(item.index + added++, 0, item.value);
    return base.exec(query.sql, ...bindings);
  };
  // Point lookups use covering oid/peeled indexes, including old live snapshots.
  view.isTip = (oid) => {
    const generation = state?.generation ?? base.exec("SELECT generation FROM repository WHERE id=1")[0].generation;
    return base.exec("SELECT 1 FROM ref_versions WHERE oid=? AND last_generation IS NULL AND first_generation<=? UNION ALL SELECT 1 FROM ref_versions WHERE peeled=? AND last_generation IS NULL AND first_generation<=? UNION ALL SELECT 1 FROM ref_versions WHERE oid=? AND last_generation>? AND first_generation<=? UNION ALL SELECT 1 FROM ref_versions WHERE peeled=? AND last_generation>? AND first_generation<=? LIMIT 1", oid, generation, oid, generation, oid, generation, generation, oid, generation, generation).length > 0;
  };
  let slot, released = false;
  view.createFetchTables = () => {
    compiled.clear();
    base.fetchFree ??= []; base.fetchActive ??= new Set();
    slot = base.fetchFree.pop(); if (slot) base.fetchActive.add(slot);
    if (slot) { for (const table of FETCH_TABLES) names.set(table, `job_${slot}_fetch_${table.slice(6)}`); return; }
    slot = owner.id.replaceAll("-", ""); base.fetchActive.add(slot);
    for (const [name, target] of names) {
      const schema = base.exec("SELECT sql FROM sqlite_master WHERE type='table' AND name=?", name)[0].sql;
      base.exec(schema.replaceAll(new RegExp(`\\b${name}\\b`, "g"), target));
      const indexes = base.exec("SELECT name,sql FROM sqlite_master WHERE type='index' AND tbl_name=? AND sql IS NOT NULL", name);
      for (const index of indexes) base.exec(index.sql.replaceAll(new RegExp(`\\b${index.name}\\b`, "g"), `${target}_${index.name}`).replaceAll(new RegExp(`\\b${name}\\b`, "g"), target));
    }
  };
  view.dropFetchTables = () => {
    if (released) return; released = true;
    for (const target of names.values()) base.exec(`DELETE FROM ${target}`);
    base.exec("DELETE FROM job_controls WHERE job=?", owner.id);
    if (slot) { base.fetchActive.delete(slot); base.fetchFree.push(slot); }
  };
  return view;
}

// Keep the newest version and every interval visible to any live job. Work per
// alarm is bounded; old log oids remain until repository deletion.
export function pruneRefVersions(store, oldestGeneration, limit = 512) {
  return store.exec("DELETE FROM ref_versions WHERE (name,first_generation) IN (SELECT name,first_generation FROM ref_versions WHERE last_generation<=? LIMIT ?) RETURNING first_generation", oldestGeneration, limit).length;
}
