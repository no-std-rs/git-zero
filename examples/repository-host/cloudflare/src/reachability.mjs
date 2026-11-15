// Only raw selectors need a global proof. Ref/path walks carry their own proof.
// This index is additive, generation-scoped, and advances in bounded batches.
import { ApiError } from "./api.mjs";
import { bytes } from "./storage.mjs";
export class ReachabilityPending extends ApiError {
  constructor(message = "reachability index is being built; retry this request") {
    super(message, 503); this.code = "reachability_index_building";
  }
}
export function proveRaw(store, generation, oid, limits) {
  const member = () => store.exec("SELECT 1 FROM browse_reachable WHERE oid=? AND generation=?", oid, generation).length;
  let state = store.exec("SELECT * FROM browse_index WHERE id=1")[0];
  if (state?.generation === generation && member()) return true;
  if (state?.generation === generation && state.phase === 2) return false;
  store.ctx.storage.transactionSync(() => {
    if (state?.generation !== generation) {
      store.exec("INSERT INTO browse_index(id,generation) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation,phase=0,after_ref=X'',objects=0,edges=0", generation);
      state = { generation, phase: 0, after_ref: new ArrayBuffer(0), objects: 0, edges: 0 };
    }
    // Stale proof rows never authorize. Reclaim them a page at a time.
    store.exec("DELETE FROM browse_reachable WHERE oid IN (SELECT oid FROM browse_reachable WHERE generation<>? LIMIT ?)", generation, limits.indexSteps);
    let work = 0;
    const insert = (id) => {
      const changed = store.exec("INSERT INTO browse_reachable(oid,generation) VALUES(?,?) ON CONFLICT(oid) DO UPDATE SET generation=excluded.generation,walked=0,relation=-1,after_dst=X'' WHERE generation<>excluded.generation RETURNING oid", id, generation).length;
      state.objects += changed;
    };
    while (work < limits.indexSteps && state.phase !== 2) {
      if (state.objects >= limits.visibleObjects || state.edges >= limits.edges) break;
      if (state.phase === 0) {
        const rows = store.exec("SELECT name,oid FROM refs WHERE name>COALESCE(?,X'') ORDER BY name LIMIT ?", state.after_ref, Math.min(64, limits.indexSteps - work, limits.visibleObjects - state.objects));
        for (const row of rows) { insert(row.oid); state.after_ref = bytes(row.name).slice().buffer; work++; }
        if (!rows.length) { state.phase = 1; work++; }
      } else {
        const item = store.exec("SELECT * FROM browse_reachable WHERE generation=? AND walked=0 ORDER BY oid LIMIT 1", generation)[0];
        if (!item) { state.phase = 2; break; }
        const row = store.published(item.oid);
        if (!row?.scanned) throw new Error("published graph metadata is absent");
        const after = item.relation < 0 ? (store.binaryEdges ? new ArrayBuffer(0) : "") : item.after_dst;
        const count = Math.min(64, limits.indexSteps - work, limits.edges - state.edges, limits.visibleObjects - state.objects);
        const links = store.exec(`SELECT dst,relation,${store.edgeTargetSQL()} AS oid FROM stage_edges WHERE stage=? AND node=? AND (relation,dst)>(?,?) ORDER BY relation,dst LIMIT ?`, store.edgeStage(row.stage), row.node, item.relation, after, count);
        for (const link of links) { insert(link.oid); state.edges++; work++; }
        if (!links.length) work++;
        const last = links.at(-1);
        if (last) store.exec("UPDATE browse_reachable SET relation=?,after_dst=?,walked=? WHERE oid=?", last.relation, store.binaryEdges ? bytes(last.dst).slice().buffer : last.dst, Number(links.length < count), item.oid);
        else store.exec("UPDATE browse_reachable SET walked=1 WHERE oid=?", item.oid);
      }
    }
    store.exec("UPDATE browse_index SET phase=?,after_ref=COALESCE(?,X''),objects=?,edges=? WHERE id=1", state.phase, state.after_ref, state.objects, state.edges);
  });
  if (member()) return true;
  if (state.phase === 2) return false;
  if (state.objects >= limits.visibleObjects || state.edges >= limits.edges) throw new ReachabilityPending("reachability index is paused at its configured budget; increase visibleObjects/edges to continue");
  throw new ReachabilityPending();
}
