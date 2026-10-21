import { bytes } from "./storage.mjs";
import { CodecError } from "./codec.mjs";

const fromHex = (id) => Uint8Array.from(id.match(/../g), (byte) => Number.parseInt(byte, 16));

function fastForward(store, stage, command, codec) {
  if (!codec) throw new Error("branch updates require ancestry validation");
  store.exec("DELETE FROM receive_walk");
  try {
    codec.ancestryStart(fromHex(command.old_oid));
    store.exec("INSERT INTO receive_walk(oid,depth) VALUES(?,0)", command.new_oid);
    for (;;) {
      const page = store.exec("SELECT oid,depth FROM receive_walk WHERE walked=0 AND depth=(SELECT MIN(depth) FROM receive_walk WHERE walked=0) ORDER BY depth,oid LIMIT 128");
      if (!page.length) return false;
      for (const item of page) {
        const row = store.object(stage, item.oid);
        if (!row) throw new Error("commit ancestry target is absent");
        if (codec.ancestryVisit(fromHex(item.oid), row.kind)) return true;
        store.exec("UPDATE receive_walk SET walked=1 WHERE oid=?", item.oid);
        store.exec("INSERT OR IGNORE INTO receive_walk(oid,depth) SELECT dst,? FROM stage_edges WHERE stage=? AND node=? AND relation=1 AND kind=1", item.depth + 1, row.stage, row.node);
      }
    }
  } finally { store.exec("DELETE FROM receive_walk"); }
}

function ancestors(store, name, ends) {
  // Bind the raw name once; SQLite slices BLOB bytes, including non-UTF-8 refs.
  return store.exec("SELECT 1 FROM proposed_refs WHERE name IN (SELECT substr(?,1,value) FROM json_each(?)) LIMIT 1", name, JSON.stringify(ends)).length > 0;
}

function conflicts(store, value) {
  const name = bytes(value);
  const ends = [];
  for (let end = 0; end < name.length; end++) {
    if (name[end] !== 47) continue;
    ends.push(end);
    // Bound both the number of indexed lookups and temporary prefix bytes per
    // statement without copying every growing prefix through the host binding.
    if (ends.length === 64) {
      if (ancestors(store, value, ends)) return true;
      ends.length = 0;
    }
  }
  if (ends.length && ancestors(store, value, ends)) return true;
  const lower = new Uint8Array(name.length + 1); lower.set(name); lower[name.length] = 47;
  const upper = lower.slice(); upper[name.length] = 48;
  // BLOB order keeps every raw-byte descendant in [name+'/', name+'0').
  return store.exec("SELECT 1 FROM proposed_refs WHERE name>=? AND name<? LIMIT 1", lower.buffer, upper.buffer).length > 0;
}

// Storage visibility is one SQLite transaction. Objects keep their immutable
// physical stage/offset keys; publication never copies the object/edge catalog.
export async function publish(store, stage, codec) {
  try {
    return store.ctx.storage.transactionSync(() => {
    const pack = store.exec("SELECT * FROM stages WHERE id=? AND phase='verified'", stage)[0];
    if (!pack) throw new Error("stage is not ready for publication");
    const commands = store.exec("SELECT * FROM stage_commands WHERE stage=? ORDER BY seq", stage);
    codec?.ancestryInit();
    const outcomes = commands.map((command) => {
      const current = store.exec("SELECT oid FROM refs WHERE name=?", command.name)[0];
      const old = command.creation ? null : command.old_oid;
      const target = command.deletion ? null : store.object(stage, command.new_oid);
      let error = null;
      if ((current?.oid ?? null) !== old) error = "stale ref";
      else if (!command.deletion && !target) error = "missing target";
      else if (target && command.branch && target.kind !== 1) error = "branch target is not a commit";
      else if (command.branch && !command.creation && !command.deletion && command.old_oid !== command.new_oid) {
        try { if (!fastForward(store, stage, command, codec)) error = "non-fast-forward"; }
        catch (failure) {
          if (!(failure instanceof CodecError) || failure.code !== -14) throw failure;
          error = "ancestry traversal limit exceeded";
        }
      }
      return { ...command, deletion: command.deletion, error };
    });
    // Stage candidate ref names in SQLite, so collision checks include planned
    // deletions and never require a repository-sized in-memory projection.
    store.exec("DELETE FROM proposed_refs");
    store.exec("INSERT INTO proposed_refs(name,seq) SELECT name,NULL FROM refs");
    for (const command of outcomes) if (!command.error && command.deletion) store.exec("DELETE FROM proposed_refs WHERE name=?", command.name);
    for (const command of outcomes) {
      if (command.error || command.deletion) continue;
      if (conflicts(store, command.name)) command.error = "ref name conflicts";
      else store.exec("INSERT OR REPLACE INTO proposed_refs(name,seq) VALUES(?,?)", command.name, command.seq);
    }
    store.exec("DELETE FROM proposed_refs");
    if ((pack.selected & 16) && outcomes.some((command) => command.error)) {
      for (const command of outcomes) command.error ??= "atomic push failed";
    }
    const accepted = outcomes.filter((command) => !command.error && command.old_oid !== command.new_oid);
    if (accepted.length) {
      // Count each new ID once, including duplicate pack entries. The global
      // OID index lets later incremental pushes probe existing published data.
      const added = store.exec("SELECT COUNT(*) AS count FROM (SELECT o.oid FROM stage_objects o WHERE o.stage=? AND NOT EXISTS(SELECT 1 FROM stage_objects p JOIN stages s ON s.id=p.stage WHERE p.oid=o.oid AND s.phase='published') GROUP BY o.oid)", stage)[0].count;
      const peeled = new Map();
      for (const command of accepted) {
        if (command.deletion) store.exec("DELETE FROM refs WHERE name=?", command.name);
        else {
          // The object snapshot is immutable throughout this transaction.
          // Cache null as well as peeled IDs, bounded by the command limit.
          if (!peeled.has(command.new_oid)) peeled.set(command.new_oid, store.peeled(command.new_oid, stage));
          store.exec("INSERT INTO refs(name,oid,peeled) VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET oid=excluded.oid,peeled=excluded.peeled", command.name, command.new_oid, peeled.get(command.new_oid));
        }
      }
      const state = store.exec("SELECT head FROM repository WHERE id=1")[0];
      let head = state.head;
      if (!store.exec("SELECT 1 FROM refs WHERE name=?", head).length) {
        const branch = accepted.find((command) => command.branch && !command.deletion);
        if (branch) head = branch.name;
      }
      // Equality proves every physical entry has a new, distinct canonical ID.
      // These segments can later form a complete pack without per-object I/O.
      if (added) {
        store.exec("UPDATE stages SET phase='published',reusable=? WHERE id=?", Number(added === pack.expected), stage);
        // Ref outcomes already own their names. These controls describe this
        // request, while objects and graph edges remain the published catalog.
        for (const table of ["stage_commands", "stage_options", "stage_shallow"]) store.exec(`DELETE FROM ${table} WHERE stage=?`, stage);
      }
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
