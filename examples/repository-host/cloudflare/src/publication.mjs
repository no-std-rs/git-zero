import { bytes } from "./storage.mjs";
import { CodecError } from "./codec.mjs";
import { repositoryPolicy } from "./policy.mjs";
import { publishedProofs } from "./reachability.mjs";

const refBytes = (name) => new TextEncoder().encode(name).buffer;
const MAIN = refBytes("refs/heads/main"), MASTER = refBytes("refs/heads/master");
const HEADS = refBytes("refs/heads/"), HEADS_END = refBytes("refs/heads0");

const fromHex = (id) => Uint8Array.from(id.match(/../g), (byte) => Number.parseInt(byte, 16));

async function fastForward(store, stage, command, codec) {
  const table = `ancestry_${crypto.randomUUID().replaceAll("-", "")}`;
  if (!codec) throw new Error("branch updates require ancestry validation");
  store.exec(`CREATE TABLE ${table}(oid TEXT PRIMARY KEY,depth INTEGER NOT NULL,walked INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID`);
  store.exec(`CREATE INDEX ${table}_work ON ${table}(depth,oid) WHERE walked=0`);
  try {
    codec.ancestryStart(fromHex(command.old_oid));
    store.exec(`INSERT INTO ${table}(oid,depth) VALUES(?,0)`, command.new_oid);
    let visited = 0;
    for (;;) {
      const page = store.exec(`SELECT oid,depth FROM ${table} WHERE walked=0 ORDER BY depth,oid LIMIT 128`);
      if (!page.length) return false;
      for (const item of page) {
        if (++visited % 128 === 0) await new Promise((resolve) => setTimeout(resolve, 0));
        if (store.env?.JOB_SIGNAL?.aborted) throw new Error("repository job cancelled");
        const row = store.object(stage, item.oid);
        if (!row) throw new Error("commit ancestry target is absent");
        if (codec.ancestryVisit(fromHex(item.oid), row.kind)) return true;
        store.exec(`UPDATE ${table} SET walked=1 WHERE oid=?`, item.oid);
        store.exec(`INSERT OR IGNORE INTO ${table}(oid,depth) SELECT ${store.edgeTargetSQL()},? FROM stage_edges WHERE stage=? AND node=? AND relation=1 AND kind=1`, item.depth + 1, store.edgeStage(row.stage), row.node);
      }
    }
  } finally { store.exec(`DROP TABLE ${table}`); }
}

function ancestors(store, name, ends) {
  const prefixes = "SELECT substr(?,1,value) FROM json_each(?)";
  return store.exec(`WITH prefixes(name) AS (${prefixes}) SELECT 1 WHERE EXISTS(SELECT 1 FROM refs r WHERE r.name IN(SELECT name FROM prefixes) AND NOT EXISTS(SELECT 1 FROM proposed_refs d WHERE d.name=r.name AND d.seq IS NULL)) OR EXISTS(SELECT 1 FROM proposed_refs p WHERE p.seq IS NOT NULL AND p.name IN(SELECT name FROM prefixes))`, name, JSON.stringify(ends)).length > 0;
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
  return store.exec("SELECT 1 WHERE EXISTS(SELECT 1 FROM refs r WHERE name>=? AND name<? AND NOT EXISTS(SELECT 1 FROM proposed_refs d WHERE d.name=r.name AND d.seq IS NULL)) OR EXISTS(SELECT 1 FROM proposed_refs WHERE seq IS NOT NULL AND name>=? AND name<?)", lower.buffer, upper.buffer, lower.buffer, upper.buffer).length > 0;
}

// Storage visibility is one SQLite transaction. Objects keep their immutable
// physical stage/offset keys; publication never copies the object/edge catalog.
export async function publish(store, stage, codec, actor = "") {
  try {
    const commands = store.exec("SELECT * FROM stage_commands WHERE stage=? ORDER BY seq", stage);
    const ancestry = new Map(); codec?.ancestryInit();
    // Immutable graph work happens outside the short ref CAS transaction.
    for (const command of commands) if (command.branch && !command.creation && !command.deletion && command.old_oid !== command.new_oid) {
      try { ancestry.set(command.seq, { additive: await fastForward(store, stage, command, codec) }); }
      catch (failure) {
        if (!(failure instanceof CodecError) || failure.code !== -14) throw failure;
        ancestry.set(command.seq, { additive: false, exhausted: true });
      }
    }
    if (store.env?.JOB_SIGNAL?.aborted) throw new Error("repository job cancelled");
    return store.ctx.storage.transactionSync(() => {
    const pack = store.exec("SELECT * FROM stages WHERE id=? AND phase='verified'", stage)[0];
    if (!pack) throw new Error("stage is not ready for publication");
    const nextGeneration = store.exec("SELECT generation+1 AS generation FROM repository WHERE id=1")[0].generation;
    const policy = repositoryPolicy(store).policy;
    const outcomes = commands.map((command) => {
      const current = store.exec("SELECT oid FROM refs WHERE name=?", command.name)[0];
      const old = command.creation ? null : command.old_oid;
      const target = command.deletion ? null : store.object(stage, command.new_oid);
      let error = null, additive = Boolean(command.creation || command.old_oid === command.new_oid);
      if ((current?.oid ?? null) !== old) error = "stale ref";
      else if (!command.deletion && !target) error = "missing target";
      else if (target && command.branch && target.kind !== 1) error = "branch target is not a commit";
      else if (command.branch && !command.creation && !command.deletion && command.old_oid !== command.new_oid) {
        const result = ancestry.get(command.seq); additive = result.additive;
        if (!policy.allowNonFastForward && !additive) error = result.exhausted ? "ancestry traversal limit exceeded" : "non-fast-forward";
      }
      return { ...command, additive, deletion: command.deletion, error };
    });
    // Stage candidate ref names in SQLite, so collision checks include planned
    // deletions and never require a repository-sized in-memory projection.
    store.exec("DELETE FROM proposed_refs");
    for (const command of outcomes) if (!command.error && command.deletion) store.exec("INSERT INTO proposed_refs(name,seq) VALUES(?,NULL)", command.name);
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
      for (const command of accepted) if (!command.creation) store.exec("UPDATE ref_versions SET last_generation=? WHERE name=? AND last_generation IS NULL", nextGeneration, command.name);
      for (const command of accepted) {
        if (command.deletion) store.exec("DELETE FROM refs WHERE name=?", command.name);
        else {
          // The object snapshot is immutable throughout this transaction.
          // Cache null as well as peeled IDs, bounded by the command limit.
          if (!peeled.has(command.new_oid)) peeled.set(command.new_oid, store.peeled(command.new_oid, stage));
          store.exec("INSERT OR IGNORE INTO ref_names(name) VALUES(?)", command.name);
          store.exec("INSERT INTO ref_versions(name,first_generation,oid,peeled) VALUES(?,?,?,?)", command.name, nextGeneration, command.new_oid, peeled.get(command.new_oid));
          store.exec("INSERT INTO refs(name,oid,peeled) VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET oid=excluded.oid,peeled=excluded.peeled", command.name, command.new_oid, peeled.get(command.new_oid));
        }
      }
      const state = store.exec("SELECT head FROM repository WHERE id=1")[0];
      const settings = store.exec("SELECT default_branch FROM host_settings WHERE id=1")[0];
      let head = settings?.default_branch ?? state.head;
      if (!settings?.default_branch && !store.exec("SELECT 1 FROM refs WHERE name=?", head).length) {
        const branch = store.exec("SELECT name FROM refs WHERE name=?", MASTER)[0]
          ?? store.exec("SELECT name FROM refs WHERE name>=? AND name<? ORDER BY name LIMIT 1", HEADS, HEADS_END)[0];
        head = branch?.name ?? MAIN;
      }
      // Equality proves every physical entry has a new, distinct canonical ID.
      // These segments can later form a complete pack without per-object I/O.
      if (added) {
        store.exec("UPDATE stages SET phase='published',reusable=?,published_generation=? WHERE id=?", Number(added === pack.expected), nextGeneration, stage);
        // Ref outcomes already own their names. These controls describe this
        // request, while objects and graph edges remain the published catalog.
        for (const table of ["stage_commands", "stage_options", "stage_shallow"]) store.exec(`DELETE FROM ${table} WHERE stage=?`, stage);
      }
      const generation = store.exec("UPDATE repository SET generation=generation+1,object_count=object_count+?,head=? WHERE id=1 RETURNING generation", added, head)[0].generation;
      publishedProofs(store, generation, accepted);
      const time = Date.now();
      store.exec("INSERT INTO host_settings(id,last_publication) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET last_publication=excluded.last_publication", time);
      // Encode the exact UTF-8 bytes: SQLite bindings in some runtimes truncate
      // text at NUL. Empty actors and embedded NUL/BOM must round-trip too.
      const actor64 = btoa(String.fromCharCode(...new TextEncoder().encode(actor))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
      store.exec("INSERT INTO publications(generation,time,actor64) VALUES(?,?,?)", generation, time, actor64);
      // Bound SQLite bindings and preserve the original command order, including
      // creations/deletions. Ref-only publications outlive private-pack cleanup.
      for (let at = 0; at < accepted.length; at += 16) {
        const page = accepted.slice(at, at + 16);
        store.exec(`INSERT INTO publication_updates(generation,seq,name,old_oid,new_oid) VALUES${page.map(() => "(?,?,?,?,?)").join(",")}`, ...page.flatMap((command) => [generation, command.seq, command.name, command.old_oid, command.new_oid]));
      }
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
