import { bytes } from "./storage.mjs";

// Storage visibility is one SQLite transaction. Objects keep their immutable
// physical stage/offset keys; publication never copies the object/edge catalog.
export async function publish(store, stage) {
  try {
    return store.ctx.storage.transactionSync(() => {
    const pack = store.exec("SELECT * FROM stages WHERE id=? AND phase='verified'", stage)[0];
    if (!pack) throw new Error("stage is not ready for publication");
    const commands = store.exec("SELECT * FROM stage_commands WHERE stage=? ORDER BY seq", stage);
    const outcomes = commands.map((command) => {
      const current = store.exec("SELECT oid FROM refs WHERE name=?", command.name)[0];
      const old = command.creation ? null : command.old_oid;
      const target = command.deletion ? null : store.object(stage, command.new_oid);
      let error = null;
      if ((current?.oid ?? null) !== old) error = "stale ref";
      else if (!command.deletion && !target) error = "missing target";
      else if (target && command.branch && target.kind !== 1) error = "branch target is not a commit";
      return { ...command, deletion: command.deletion, error };
    });
    // Stage candidate ref names in SQLite, so collision checks include planned
    // deletions and never require a repository-sized in-memory projection.
    store.exec("DELETE FROM proposed_refs");
    store.exec("INSERT INTO proposed_refs(name,seq) SELECT name,NULL FROM refs");
    for (const command of outcomes) if (!command.error && command.deletion) store.exec("DELETE FROM proposed_refs WHERE name=?", command.name);
    for (const command of outcomes) {
      if (command.error || command.deletion) continue;
      const conflict = store.exec("SELECT name FROM proposed_refs WHERE name<>? AND ((length(name)<length(?) AND substr(?,1,length(name))=name AND substr(?,length(name)+1,1)=x'2f') OR (length(name)>length(?) AND substr(name,1,length(?))=? AND substr(name,length(?)+1,1)=x'2f')) LIMIT 1", command.name, command.name, command.name, command.name, command.name, command.name, command.name, command.name)[0];
      if (conflict) command.error = "ref name conflicts";
      else store.exec("INSERT OR REPLACE INTO proposed_refs(name,seq) VALUES(?,?)", command.name, command.seq);
    }
    store.exec("DELETE FROM proposed_refs");
    if ((pack.selected & 16) && outcomes.some((command) => command.error)) {
      for (const command of outcomes) command.error ??= "atomic push failed";
    }
    const accepted = outcomes.filter((command) => !command.error);
    if (accepted.length) {
      // Count each new ID once, including duplicate pack entries. The global
      // OID index lets later incremental pushes probe existing published data.
      const added = store.exec("SELECT COUNT(*) AS count FROM (SELECT o.oid FROM stage_objects o WHERE o.stage=? AND NOT EXISTS(SELECT 1 FROM stage_objects p JOIN stages s ON s.id=p.stage WHERE p.oid=o.oid AND s.phase='published') GROUP BY o.oid)", stage)[0].count;
      for (const command of accepted) {
        if (command.deletion) store.exec("DELETE FROM refs WHERE name=?", command.name);
        else store.exec("INSERT INTO refs(name,oid,peeled) VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET oid=excluded.oid,peeled=excluded.peeled", command.name, command.new_oid, store.peeled(command.new_oid, stage));
      }
      const state = store.exec("SELECT head FROM repository WHERE id=1")[0];
      let head = state.head;
      if (!store.exec("SELECT 1 FROM refs WHERE name=?", head).length) {
        const branch = accepted.find((command) => command.branch && !command.deletion);
        if (branch) head = branch.name;
      }
      // Equality proves every physical entry has a new, distinct canonical ID.
      // These segments can later form a complete pack without per-object I/O.
      if (pack.expected) store.exec("UPDATE stages SET phase='published',reusable=? WHERE id=?", Number(added === pack.expected), stage);
      store.exec("UPDATE repository SET generation=generation+1,object_count=object_count+?,head=? WHERE id=1", added, head);
    }
    return outcomes.map(({ name, error }) => ({ name: bytes(name), error }));
    });
  } catch (error) {
    // transactionSync rolls back before throwing. Reclaim the still-private
    // pack now; abandon retains ownership records and schedules retries if
    // storage cleanup itself fails.
    await store.abandon(stage);
    throw error;
  }
}
