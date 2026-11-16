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
  view.base = base; view.jobId = owner.id; view.snapshot = state;
  view.env = { ...base.env, JOB_SIGNAL: owner.abort.signal, JOB_OWNER: owner };
  if (fetch) for (const table of FETCH_TABLES) names.set(table, `job_${owner.id.replaceAll("-", "")}_${table}`);
  const substitute = (sql, bindings) => {
    const injected = [];
    for (const [name, target] of names) sql = sql.replaceAll(new RegExp(`\\b${name}\\b`, "g"), target);
    if (state && /^\s*(SELECT|WITH)\b/i.test(sql)) {
      const refs = `(SELECT v.name,v.oid,v.peeled FROM ref_names n JOIN ref_versions v ON v.name=n.name AND v.first_generation=(SELECT MAX(w.first_generation) FROM ref_versions w WHERE w.name=n.name AND w.first_generation<=${state.generation}) WHERE v.last_generation IS NULL OR v.last_generation>${state.generation})`;
      sql = sql.replaceAll(/\brefs\b/g, refs);
      sql = sql.replaceAll(/\brepository\b/g, (match, offset, original) => {
        injected.push({ index: (original.slice(0, offset).match(/\?/g) ?? []).length, value: state.head });
        return `(SELECT id,format,${state.generation} AS generation,${state.object_count} AS object_count,? AS head FROM repository WHERE id=1)`;
      });
      sql = sql.replaceAll(/\brepository_security\b/g, `(SELECT id,${literal(state.epoch)} AS epoch,${state.proof_floor} AS proof_floor FROM repository_security WHERE id=1)`);
      sql = sql.replaceAll(/\bstages\b/g, `(SELECT * FROM stages WHERE phase<>'published' OR published_generation<=${state.generation})`);
    }
    const values = [...bindings]; let added = 0;
    for (const item of injected) values.splice(item.index + added++, 0, item.value);
    return { sql, values };
  };
  view.exec = (sql, ...bindings) => { const query = substitute(sql, bindings); return base.exec(query.sql, ...query.values); };
  let slot, released = false;
  view.createFetchTables = () => {
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
