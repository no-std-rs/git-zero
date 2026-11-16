// Only raw selectors need a global proof. Ref/path walks carry their own proof.
// This index is additive, generation-scoped, and advances in bounded batches.
import { ApiError } from "./api.mjs";
import { bytes } from "./storage.mjs";
export class ReachabilityPending extends ApiError {
  constructor(message = "reachability index is being built; retry this request") {
    super(message, 503); this.code = "reachability_index_building";
  }
}
// Called inside the ref transaction. Existing membership and unfinished work
// survive creations/verified fast-forwards; only destructive updates revoke it.
export function publishedProofs(store, generation, updates) {
  const destructive = updates.some((command) => !command.additive);
  if (destructive) store.exec("UPDATE repository_security SET proof_floor=? WHERE id=1", generation);
  const lineage = store.exec("SELECT proof_floor FROM repository_security WHERE id=1")[0].proof_floor;
  const state = store.exec("SELECT * FROM browse_progress WHERE id=1")[0];
  if (!state || state.lineage !== lineage) {
    store.exec("INSERT INTO browse_progress(id,generation,lineage) VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation,lineage=excluded.lineage,phase=0,after_ref=X'',objects=0,edges=0", generation, lineage);
  }
  const tips = [...new Set(updates.filter((command) => !command.deletion).map((command) => command.new_oid))];
  let added = 0;
  for (let at = 0; at < tips.length; at += 32) {
    const page = tips.slice(at, at + 32);
    added += store.exec(`INSERT INTO browse_members(oid,lineage,depth,proof_generation) VALUES${page.map(() => "(?,?,0,?)").join(",")} ON CONFLICT(oid) DO UPDATE SET lineage=excluded.lineage,proof_generation=excluded.proof_generation,depth=0,walked=0,relation=-1,after_dst=X'' WHERE lineage<>excluded.lineage RETURNING oid`, ...page.flatMap((oid) => [oid, lineage, generation])).length;
    store.exec(`UPDATE browse_members SET depth=0 WHERE oid IN (${page.map(() => "?").join(",")}) AND lineage=? AND walked=0 AND depth>0`, ...page, lineage);
  }
  store.exec("UPDATE browse_progress SET generation=?,objects=objects+?,phase=CASE WHEN phase=2 THEN 1 ELSE phase END WHERE id=1", generation, added);
}
export function advanceIndex(store, generation, limits) {
  const lineage = store.exec("SELECT proof_floor FROM repository_security WHERE id=1")[0].proof_floor;
  let state = store.exec("SELECT * FROM browse_progress WHERE id=1")[0];
  if (state?.generation === generation && state.phase === 2) return state;
  store.ctx.storage.transactionSync(() => {
    if (state?.generation !== generation) {
      const retained = state?.lineage === lineage;
      store.exec("INSERT INTO browse_progress(id,generation,lineage,objects,edges) VALUES(1,?,?,?,?) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation,lineage=excluded.lineage,phase=0,after_ref=X'',objects=excluded.objects,edges=excluded.edges", generation, lineage, retained ? state.objects : 0, retained ? state.edges : 0);
      state = { generation, lineage, phase: 0, after_ref: new ArrayBuffer(0), objects: retained ? state.objects : 0, edges: retained ? state.edges : 0 };
    }
    // Stale rows cannot authorize; reclaim them within this request's budget.
    store.exec("DELETE FROM browse_members WHERE oid IN (SELECT oid FROM browse_members WHERE lineage<>? LIMIT ?)", lineage, limits.indexSteps);
    let work = 0;
    const insert = (ids, depth, proofGeneration) => {
      if (!ids.length) return;
      const changed = store.exec(`INSERT INTO browse_members(oid,lineage,depth,proof_generation) VALUES${ids.map(() => "(?,?,?,?)").join(",")} ON CONFLICT(oid) DO UPDATE SET lineage=excluded.lineage,proof_generation=excluded.proof_generation,depth=excluded.depth,walked=0,relation=-1,after_dst=X'' WHERE lineage<>excluded.lineage RETURNING oid`, ...ids.flatMap((id) => [id, lineage, depth, proofGeneration])).length;
      state.objects += changed;
    };
    while (work < limits.indexSteps && state.phase !== 2) {
      if (state.objects >= limits.visibleObjects || state.edges >= limits.edges) break;
      if (state.phase === 0) {
        const rows = store.exec("SELECT name,oid FROM refs WHERE name>COALESCE(?,X'') ORDER BY name LIMIT ?", state.after_ref, Math.min(32, limits.indexSteps - work, limits.visibleObjects - state.objects));
        insert(rows.map((row) => row.oid), 0, generation);
        if (rows.length) state.after_ref = bytes(rows.at(-1).name).slice().buffer;
        work += rows.length;
        if (!rows.length) { state.phase = 1; work++; }
      } else {
        const item = store.exec("SELECT * FROM browse_members WHERE lineage=? AND walked=0 ORDER BY depth,oid LIMIT 1", lineage)[0];
        if (!item) { state.phase = 2; break; }
        const row = store.published(item.oid);
        if (!row?.scanned) throw new Error("published graph metadata is absent");
        const after = item.relation < 0 ? (store.binaryEdges ? new ArrayBuffer(0) : "") : item.after_dst;
        const count = Math.min(32, limits.indexSteps - work, limits.edges - state.edges, limits.visibleObjects - state.objects);
        const links = store.exec(`SELECT dst,relation,${store.edgeTargetSQL()} AS oid FROM stage_edges WHERE stage=? AND node=? AND (relation,dst)>(?,?) ORDER BY relation,dst LIMIT ?`, store.edgeStage(row.stage), row.node, item.relation, after, count);
        insert(links.map((link) => link.oid), item.depth + 1, item.proof_generation);
        state.edges += links.length; work += links.length;
        if (!links.length) work++;
        const last = links.at(-1);
        if (last) store.exec("UPDATE browse_members SET relation=?,after_dst=?,walked=? WHERE oid=?", last.relation, store.binaryEdges ? bytes(last.dst).slice().buffer : last.dst, Number(links.length < count), item.oid);
        else store.exec("UPDATE browse_members SET walked=1 WHERE oid=?", item.oid);
      }
    }
    store.exec("UPDATE browse_progress SET phase=?,after_ref=COALESCE(?,X''),objects=?,edges=? WHERE id=1", state.phase, state.after_ref, state.objects, state.edges);
  });
  return state;
}
export function proveRaw(store, generation, oid, limits) {
  const floor = store.exec("SELECT proof_floor FROM repository_security WHERE id=1")[0].proof_floor;
  const member = () => store.exec("SELECT 1 FROM browse_members WHERE oid=? AND lineage=? AND proof_generation<=?", oid, floor, generation).length;
  if (member()) return true;
  const current = (store.base ?? store).exec("SELECT generation FROM repository WHERE id=1")[0].generation;
  if (current !== generation) throw new ReachabilityPending("snapshot reachability proof is unavailable; retry against current refs");
  const state = advanceIndex(store, generation, limits);
  if (member()) return true;
  if (state.phase === 2) return false;
  if (state.objects >= limits.visibleObjects || state.edges >= limits.edges) throw new ReachabilityPending("reachability index is paused at its configured budget; increase visibleObjects/edges to continue");
  throw new ReachabilityPending();
}
