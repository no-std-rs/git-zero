// Durable storage for opaque codec records and authenticated byte chunks.
import { bucket } from "./r2-s3.mjs";
import { hex } from "./codec.mjs";
import { RequestError, storageFailure } from "./errors.mjs";
export const CHUNK_BYTES = 1024 * 1024;
const MAX_STORED_CHUNK_BYTES = 8 * 1024 * 1024;
export const PAGE_BYTES = 65536;
export const FETCH_TABLES = ["fetch_wants", "fetch_haves", "fetch_shallow", "fetch_probe", "fetch_common", "fetch_selected"];

async function retryStorage(operation, usage) {
  for (let attempt = 0; ; attempt++) {
    try { return await operation(); }
    catch (error) {
      const transient = error?.retryable === true || /network connection lost|network error|fetch failed|ECONNRESET|ETIMEDOUT/i.test(String(error?.message ?? error));
      if (!transient || attempt === 2) throw storageFailure(error);
      usage.r2Retries++;
      await new Promise((resolve) => setTimeout(resolve, 100 * 4 ** attempt));
    }
  }
}

export function blob(bytes) { return bytes.slice().buffer; }
export function bytes(value) { return value instanceof Uint8Array ? value : new Uint8Array(value); }
export async function sha256(value) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", value));
  return hex(digest);
}

// A group of caches can share complete owned buffers (plain intake seeds do).
// Release backing storage when the last cache owner retires it, independently
// of when the runtime collects the retired JavaScript buffer wrappers.
export class CacheBuffers {
  constructor() { this.references = new WeakMap(); }
  retain(value) {
    if (value.byteOffset !== 0 || value.byteLength !== value.buffer.byteLength) throw new TypeError("owned cache requires a complete buffer");
    this.references.set(value.buffer, (this.references.get(value.buffer) ?? 0) + 1);
  }
  release(value) {
    const count = this.references.get(value.buffer);
    if (count === undefined) throw new Error("cache buffer has no owner");
    if (count === 1) { this.references.delete(value.buffer); value.buffer.transfer(0); }
    else this.references.set(value.buffer, count - 1);
  }
}

export class ByteCache {
  constructor(limit, maxEntries = 8192, ownership = null) { this.limit = limit; this.maxEntries = maxEntries; this.ownership = ownership; this.size = 0; this.peak = 0; this.entries = new Map(); }
  get(key) {
    const value = this.entries.get(key);
    if (value === undefined) return undefined;
    this.entries.delete(key); this.entries.set(key, value); return value;
  }
  put(key, value) {
    if (!(value instanceof Uint8Array)) throw new TypeError("cache stores bytes");
    if (value.byteLength > this.limit || this.maxEntries === 0) return;
    this.ownership?.retain(value);
    const old = this.entries.get(key);
    if (old !== undefined) { this.entries.delete(key); this.size -= old.byteLength; this.ownership?.release(old); }
    while (this.size + value.byteLength > this.limit || this.entries.size >= this.maxEntries) {
      const first = this.entries.keys().next().value;
      const retired = this.entries.get(first);
      this.size -= retired.byteLength; this.entries.delete(first); this.ownership?.release(retired);
    }
    this.entries.set(key, value); this.size += value.byteLength; this.peak = Math.max(this.peak, this.size);
  }
  clear() {
    for (const value of this.entries.values()) this.ownership?.release(value);
    this.entries.clear(); this.size = 0;
  }
}

const SCHEMA = [
  "CREATE TABLE IF NOT EXISTS host_identity (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL)",
  "CREATE TABLE IF NOT EXISTS repository (id INTEGER PRIMARY KEY, format INTEGER NOT NULL, generation INTEGER NOT NULL DEFAULT 0, object_count INTEGER NOT NULL DEFAULT 0, head BLOB NOT NULL)",
  "CREATE TABLE IF NOT EXISTS stages (id TEXT PRIMARY KEY, format INTEGER NOT NULL, phase TEXT NOT NULL, retained INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, pack TEXT, bytes INTEGER NOT NULL DEFAULT 0, selected INTEGER NOT NULL DEFAULT 0, expected INTEGER NOT NULL DEFAULT 0, reusable INTEGER NOT NULL DEFAULT 0, has_ofs INTEGER NOT NULL DEFAULT 1) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_commands (stage TEXT NOT NULL, seq INTEGER NOT NULL, name BLOB NOT NULL, old_oid TEXT NOT NULL, new_oid TEXT NOT NULL, branch INTEGER NOT NULL, creation INTEGER NOT NULL, deletion INTEGER NOT NULL, PRIMARY KEY(stage,seq), UNIQUE(stage,name)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_options (stage TEXT NOT NULL, seq INTEGER NOT NULL, value BLOB NOT NULL, PRIMARY KEY(stage,seq)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_shallow (stage TEXT NOT NULL, oid TEXT NOT NULL, PRIMARY KEY(stage,oid)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_chunks (stage TEXT NOT NULL, seq INTEGER NOT NULL, key TEXT NOT NULL, start INTEGER NOT NULL, length INTEGER NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(stage,seq)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS stage_chunk_offsets ON stage_chunks(stage,start)",
  "CREATE INDEX IF NOT EXISTS stage_chunk_keys ON stage_chunks(key,stage)",
  "CREATE TABLE IF NOT EXISTS stage_objects (stage TEXT NOT NULL, node INTEGER NOT NULL, wire_kind INTEGER NOT NULL, packed_size INTEGER NOT NULL, compressed_offset INTEGER NOT NULL, end_offset INTEGER, crc INTEGER, wire_base_offset INTEGER, wire_base_id TEXT, oid TEXT, kind INTEGER, size INTEGER, depth INTEGER, base_stage TEXT, base_node INTEGER, base_oid TEXT, verified INTEGER NOT NULL DEFAULT 0, scanned INTEGER NOT NULL DEFAULT 0, ready INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(stage,node)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS stage_object_ids ON stage_objects(stage,oid)",
  "CREATE INDEX IF NOT EXISTS published_object_ids ON stage_objects(oid,depth,stage)",
  "CREATE INDEX IF NOT EXISTS stage_object_work ON stage_objects(stage,node) WHERE verified=1 AND scanned=0",
  "CREATE INDEX IF NOT EXISTS stage_ready_work ON stage_objects(stage,node) WHERE verified=0 AND ready=1",
  "CREATE INDEX IF NOT EXISTS stage_offset_dependencies ON stage_objects(stage,wire_base_offset) WHERE verified=0 AND wire_base_offset IS NOT NULL",
  "CREATE INDEX IF NOT EXISTS stage_id_dependencies ON stage_objects(stage,wire_base_id) WHERE verified=0 AND wire_base_id IS NOT NULL",
  "CREATE TABLE IF NOT EXISTS stage_edges (stage INTEGER NOT NULL, node INTEGER NOT NULL, dst BLOB NOT NULL, kind INTEGER NOT NULL, relation INTEGER NOT NULL, PRIMARY KEY(stage,node,relation,dst)) WITHOUT ROWID",
  // One immutable target cannot satisfy two expected kinds. Check conflicts
  // before OR IGNORE can discard them; this also protects existing schemas.
  "CREATE TRIGGER IF NOT EXISTS stage_edge_kind BEFORE INSERT ON stage_edges WHEN EXISTS(SELECT 1 FROM stage_edges WHERE stage=NEW.stage AND node=NEW.node AND relation=NEW.relation AND dst=NEW.dst AND kind<>NEW.kind) BEGIN SELECT RAISE(ABORT,'conflicting graph target kinds'); END",
  "CREATE TABLE IF NOT EXISTS stage_names (stage TEXT NOT NULL, node INTEGER NOT NULL, name BLOB NOT NULL, mode INTEGER NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, PRIMARY KEY(stage,node,name)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_blob_checks (stage TEXT NOT NULL, oid TEXT NOT NULL, checks INTEGER NOT NULL DEFAULT 2, PRIMARY KEY(stage,oid)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS temp_chunks (stage TEXT NOT NULL, object TEXT NOT NULL, seq INTEGER NOT NULL, key TEXT NOT NULL, start INTEGER NOT NULL, length INTEGER NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(stage,object,seq)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS temp_chunk_offsets ON temp_chunks(stage,object,start)",
  "CREATE TABLE IF NOT EXISTS temp_objects (stage TEXT NOT NULL, object TEXT NOT NULL, oid TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY(stage,object)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS refs (name BLOB PRIMARY KEY, oid TEXT NOT NULL, peeled TEXT) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS proposed_refs (name BLOB PRIMARY KEY, seq INTEGER) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS receive_walk (oid TEXT PRIMARY KEY, depth INTEGER NOT NULL, walked INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS receive_walk_work ON receive_walk(depth,oid) WHERE walked=0",
  "CREATE TABLE IF NOT EXISTS fetch_wants (oid TEXT PRIMARY KEY) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS fetch_haves (oid TEXT PRIMARY KEY) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS fetch_shallow (oid TEXT PRIMARY KEY) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS fetch_probe (oid TEXT PRIMARY KEY, walked INTEGER NOT NULL DEFAULT 0, depth INTEGER NOT NULL DEFAULT 1, stage TEXT, node INTEGER) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS fetch_probe_depth ON fetch_probe(depth,oid) WHERE walked=0",
  "CREATE TABLE IF NOT EXISTS fetch_common (oid TEXT PRIMARY KEY, walked INTEGER NOT NULL DEFAULT 0, stage TEXT, node INTEGER) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS fetch_common_work ON fetch_common(walked,oid)",
  "CREATE TABLE IF NOT EXISTS fetch_selected (oid TEXT PRIMARY KEY, walked INTEGER NOT NULL DEFAULT 0, stage TEXT, node INTEGER, depth INTEGER NOT NULL DEFAULT 1, included INTEGER NOT NULL DEFAULT 1, shallow INTEGER NOT NULL DEFAULT 0, unshallow INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS fetch_selected_physical ON fetch_selected(stage,node)",
];
const STAGE_METADATA = ["stage_objects", "stage_edges", "stage_names", "stage_blob_checks", "stage_commands", "stage_options", "stage_shallow"];

export class Store {
  constructor(ctx, env, namespace) {
    this.ctx = ctx; this.env = env; this.namespace = namespace; this.blobs = bucket(env);
    this.usage = { sqlCalls: 0, sqlRowsRead: 0, sqlRowsWritten: 0, r2Gets: 0, r2Puts: 0, r2Deletes: 0, r2Retries: 0, r2ReadBytes: 0, r2WrittenBytes: 0 };
    this.sqlKnown = true;
    this.edgeStages = new Map();
    for (const statement of SCHEMA) this.exec(statement);
    // Existing text catalogs retain their layout. Fresh catalogs intern the
    // opaque stage handle once instead of repeating its UUID in every edge.
    const edges = this.exec("PRAGMA table_info(stage_edges)");
    this.compactEdges = edges.find((column) => column.name === "stage").type === "INTEGER";
    this.binaryEdges = edges.find((column) => column.name === "dst").type === "BLOB";
    if (this.compactEdges) {
      this.exec("CREATE TABLE IF NOT EXISTS edge_stages(id INTEGER PRIMARY KEY,stage TEXT NOT NULL UNIQUE)");
      this.exec("CREATE TRIGGER IF NOT EXISTS stage_edge_owner BEFORE INSERT ON stage_edges WHEN typeof(NEW.stage)<>'integer' BEGIN SELECT RAISE(ABORT,'graph stage ID is invalid'); END");
    }
    if (this.binaryEdges) this.exec("CREATE TRIGGER IF NOT EXISTS stage_edge_id BEFORE INSERT ON stage_edges WHEN typeof(NEW.dst)<>'blob' OR length(NEW.dst) NOT IN (20,32) BEGIN SELECT RAISE(ABORT,'graph object ID is invalid'); END");
    // Earlier pending checks selected attributes only. Preserve that meaning.
    if (!this.exec("PRAGMA table_info(stage_blob_checks)").some((column) => column.name === "checks")) this.exec("ALTER TABLE stage_blob_checks ADD COLUMN checks INTEGER NOT NULL DEFAULT 2");
    // Older stages have unknown entry kinds; conservatively disable raw OFS reuse.
    if (!this.exec("PRAGMA table_info(stages)").some((column) => column.name === "has_ofs")) this.exec("ALTER TABLE stages ADD COLUMN has_ofs INTEGER NOT NULL DEFAULT 1");
    if (!this.exec("PRAGMA table_info(stages)").some((column) => column.name === "reusable")) this.exec("ALTER TABLE stages ADD COLUMN reusable INTEGER NOT NULL DEFAULT 0");
    if (!this.exec("PRAGMA table_info(refs)").some((column) => column.name === "peeled")) this.ctx.storage.transactionSync(() => {
      this.exec("ALTER TABLE refs ADD COLUMN peeled TEXT");
      for (const ref of this.references()) this.exec("UPDATE refs SET peeled=? WHERE name=?", this.peeled(ref.oid), blob(ref.name));
    });
    this.exec("CREATE INDEX IF NOT EXISTS refs_oid ON refs(oid)");
    this.exec("CREATE INDEX IF NOT EXISTS refs_peeled ON refs(peeled)");
    const frontier = this.exec("PRAGMA table_info(fetch_selected)");
    this.ctx.storage.transactionSync(() => {
      for (const [name, value] of [["depth", 1], ["included", 1], ["shallow", 0], ["unshallow", 0]]) {
        if (!frontier.some((column) => column.name === name)) this.exec(`ALTER TABLE fetch_selected ADD COLUMN ${name} INTEGER NOT NULL DEFAULT ${value}`);
      }
      this.exec("DROP INDEX IF EXISTS fetch_selected_work");
      this.exec("CREATE INDEX IF NOT EXISTS fetch_selected_depth ON fetch_selected(depth,oid) WHERE walked=0");
    });
    this.exec("INSERT OR IGNORE INTO host_identity(id,namespace) VALUES(1,?)", namespace);
    if (this.exec("SELECT namespace FROM host_identity WHERE id=1")[0].namespace !== namespace) throw new Error("stored repository identity differs");
  }
  exec(statement, ...bindings) {
    try {
      const cursor = this.ctx.storage.sql.exec(statement, ...bindings);
      const rows = cursor.toArray();
      this.usage.sqlCalls++;
      if (typeof cursor.rowsRead === "number" && typeof cursor.rowsWritten === "number") {
        this.usage.sqlRowsRead += cursor.rowsRead; this.usage.sqlRowsWritten += cursor.rowsWritten;
      } else this.sqlKnown = false;
      return rows;
    } catch (error) {
      throw storageFailure(error);
    }
  }
  initialize(format) {
    if (format !== 0 && format !== 1) throw new RangeError("unsupported object format");
    const current = this.exec("SELECT * FROM repository WHERE id=1")[0];
    if (current && current.format !== format) throw new RequestError("repository object format differs");
    if (!current) this.exec("INSERT INTO repository(id,format,generation,head) VALUES(1,?,0,?)", format, blob(new TextEncoder().encode("refs/heads/main")));
  }
  stage(format, retained = false) {
    this.initialize(format);
    const id = crypto.randomUUID();
    let graph;
    this.ctx.storage.transactionSync(() => {
      this.exec("INSERT INTO stages(id,format,phase,created,retained) VALUES(?,?,?,?,?)", id, format, "receiving", Date.now(), Number(retained));
      if (this.compactEdges) graph = this.exec("INSERT INTO edge_stages(stage) VALUES(?) RETURNING id", id)[0].id;
    });
    if (this.compactEdges) this.rememberEdgeStage(id, graph);
    return id;
  }
  rememberEdgeStage(stage, id) {
    if (!Number.isSafeInteger(id) || id <= 0) throw new Error("invalid graph stage ID");
    if (!this.edgeStages.has(stage) && this.edgeStages.size >= 256) this.edgeStages.delete(this.edgeStages.keys().next().value);
    this.edgeStages.set(stage, id);
  }
  edgeStage(stage) {
    if (!this.compactEdges) return stage;
    const cached = this.edgeStages.get(stage);
    if (cached !== undefined) return cached;
    const row = this.exec("SELECT id FROM edge_stages WHERE stage=?", stage)[0];
    if (!row) throw new Error("graph stage is absent");
    this.rememberEdgeStage(stage, row.id);
    return row.id;
  }
  edgeTarget(oid) {
    return this.binaryEdges ? Uint8Array.from(oid.match(/../g), (pair) => Number.parseInt(pair, 16)).buffer : oid;
  }
  edgeTargetSQL(column = "dst") {
    return this.binaryEdges ? `lower(hex(${column}))` : column;
  }
  node(stage, node) { return this.exec("SELECT * FROM stage_objects WHERE stage=? AND node=?", stage, node)[0]; }
  object(stage, oid) {
    return this.exec("SELECT * FROM stage_objects INDEXED BY stage_object_ids WHERE stage=? AND oid=? AND verified=1 LIMIT 1", stage, oid)[0]
      ?? this.published(oid);
  }
  published(oid) {
    return this.exec("SELECT o.* FROM stage_objects o JOIN stages s ON s.id=o.stage WHERE o.oid=? AND s.phase='published' AND o.verified=1 ORDER BY o.depth,o.stage LIMIT 1", oid)[0];
  }
  peeled(oid, stage = null) {
    const find = (id) => stage ? this.object(stage, id) : this.published(id);
    let row = find(oid);
    // Brent's cycle detection keeps constant scratch space without imposing
    // a semantic depth limit on an already verified immutable tag chain.
    let checkpoint = oid, power = 1, length = 0;
    while (row?.kind === 4) {
      const edge = this.exec(`SELECT ${this.edgeTargetSQL()} AS dst FROM stage_edges WHERE stage=? AND node=? AND relation=2 LIMIT 1`, this.edgeStage(row.stage), row.node)[0];
      if (!edge) throw new Error("stored tag has no target");
      row = find(edge.dst);
      if (row?.oid === checkpoint) throw new Error("stored tag cycle");
      if (++length === power) { checkpoint = row?.oid; power *= 2; length = 0; }
    }
    if (!row) throw new Error("tag target is absent");
    return row.oid === oid ? null : row.oid;
  }
  *references() {
    let after = new Uint8Array();
    for (;;) {
      const rows = this.exec("SELECT name,oid,peeled FROM refs WHERE name>? ORDER BY name LIMIT 128", blob(after));
      if (!rows.length) return;
      for (const row of rows) { after = bytes(row.name); yield { ...row, name: after }; }
    }
  }
  statistics() {
    const result = {
      objects: this.exec("SELECT object_count FROM repository WHERE id=1")[0]?.object_count ?? 0,
      stagedObjects: this.exec("SELECT COUNT(*) AS count FROM stage_objects")[0].count,
      stagedChunks: this.exec("SELECT COUNT(*) AS count FROM stage_chunks")[0].count,
      temporaryChunks: this.exec("SELECT COUNT(*) AS count FROM temp_chunks")[0].count,
      temporaryObjects: this.exec("SELECT COUNT(*) AS count FROM temp_objects")[0].count,
      treeNameScratchRows: this.exec("SELECT COUNT(*) AS count FROM stage_names")[0].count,
      quarantineRows: STAGE_METADATA.reduce((count, table) => count + (table === "stage_edges" && this.compactEdges
        ? this.exec("SELECT COUNT(*) AS count FROM stage_edges WHERE stage IN(SELECT g.id FROM edge_stages g JOIN stages s ON s.id=g.stage WHERE s.phase='failed')")[0].count
        : this.exec(`SELECT COUNT(*) AS count FROM ${table} d JOIN stages s ON s.id=d.stage WHERE s.phase='failed'`)[0].count), 0),
      refs: this.exec("SELECT COUNT(*) AS count FROM refs")[0].count,
      stages: this.exec("SELECT id,format,phase,bytes,expected FROM stages ORDER BY created DESC LIMIT 64"),
      databaseBytes: this.ctx.storage.sql.databaseSize ?? null,
      workerCpuMs: null,
      lastIntakeBudget: this.lastIntakeBudget ?? null,
    };
    result.usage = { ...this.usage, sqlRowsRead: this.sqlKnown ? this.usage.sqlRowsRead : null, sqlRowsWritten: this.sqlKnown ? this.usage.sqlRowsWritten : null };
    return result;
  }
  async put(key, data) {
    await retryStorage(async () => {
      this.usage.r2Puts++; this.usage.r2WrittenBytes += data.byteLength;
      await this.blobs.put(key, data);
    }, this.usage);
  }
  async delete(keys) {
    await retryStorage(async () => {
      this.usage.r2Deletes += Array.isArray(keys) ? keys.length : 1;
      await this.blobs.delete(keys);
    }, this.usage);
  }
  async get(chunk, cache) {
    const hit = cache.get(chunk.key);
    if (hit) return hit;
    const data = await retryStorage(async () => {
      this.usage.r2Gets++;
      const object = await this.blobs.get(chunk.key);
      if (!object || object.size !== chunk.length || object.size > MAX_STORED_CHUNK_BYTES) throw new Error("missing or invalid stored chunk");
      return new Uint8Array(await object.arrayBuffer());
    }, this.usage);
    this.usage.r2ReadBytes += data.byteLength;
    if (await sha256(data) !== chunk.sha) throw storageFailure(new Error("stored chunk hash differs"));
    cache.put(chunk.key, data); return data;
  }
  async clearTemporary(stage, { defer = false } = {}) {
    try {
    for (;;) {
      const rows = this.exec("SELECT object,seq,key FROM temp_chunks WHERE stage=? LIMIT 1000", stage);
      if (!rows.length) break;
      const batch = rows.map((row) => row.key);
      await this.delete(batch);
      for (const row of rows) this.exec("DELETE FROM temp_chunks WHERE stage=? AND object=? AND seq=?", stage, row.object, row.seq);
    }
    this.exec("DELETE FROM temp_objects WHERE stage=?", stage);
    return true;
    } catch (error) {
      await this.scheduleRecovery();
      if (!defer) throw error;
      // Ownership rows remain durable until deletion succeeds. Only defer
      // after the recovery alarm has itself been scheduled successfully.
      console.error(JSON.stringify({ event: "temporary-cleanup-deferred", stage, error: String(error.message ?? error) }));
      return false;
    }
  }
  async abandon(stage, { defer = false } = {}) {
    try {
    const current = this.exec("SELECT phase FROM stages WHERE id=?", stage)[0];
    if (!current) return;
    if (current.phase === "published") throw new Error("cannot abandon published storage");
    this.exec("UPDATE stages SET phase='failed' WHERE id=?", stage);
      await this.clearTemporary(stage);
      for (;;) {
        const rows = this.exec("SELECT c.seq,c.key,NOT EXISTS(SELECT 1 FROM stage_chunks other WHERE other.key=c.key AND other.stage<>c.stage) AS unshared FROM stage_chunks c WHERE c.stage=? LIMIT 128", stage);
        if (!rows.length) break;
        // Claims precede R2 puts. Keep the entire batch's ownership records
        // until deleting its unshared bytes succeeds, including on partial loss.
        const keys = rows.filter((row) => row.unshared).map((row) => row.key);
        if (keys.length) await this.delete(keys);
        for (const row of rows) this.exec("DELETE FROM stage_chunks WHERE stage=? AND seq=?", stage, row.seq);
      }
      for (const table of STAGE_METADATA) {
        const statement = table === "stage_edges" && this.compactEdges
          ? "DELETE FROM stage_edges WHERE stage=(SELECT id FROM edge_stages WHERE stage=?)"
          : `DELETE FROM ${table} WHERE stage=?`;
        this.exec(statement, stage);
      }
      if (this.compactEdges) {
        this.exec("DELETE FROM edge_stages WHERE stage=?", stage);
        this.edgeStages.delete(stage);
      }
      this.exec("DELETE FROM stages WHERE id=? AND phase='failed'", stage);
    } catch (error) {
      await this.scheduleRecovery();
      if (!defer) throw error;
      console.error(JSON.stringify({ event: "cleanup-deferred", stage, error: String(error.message ?? error) }));
    }
  }
  async scheduleRecovery() {
    try { await this.ctx.storage.setAlarm(Date.now() + 60000); }
    catch (error) { throw storageFailure(error); }
  }
  async recover() {
    // Fetch frontiers belong to a single request. A crashed stream cannot run
    // its finally block, and advertisements must not retain that old work.
    for (const table of FETCH_TABLES) this.exec(`DELETE FROM ${table}`);
    // A new durable instance has no previous in-flight writer. Private decoded
    // stages can be abandoned; verified test fixtures and published packs stay.
    for (;;) {
      const temporary = this.exec("SELECT stage FROM temp_chunks LIMIT 1")[0];
      if (!temporary) break;
      await this.clearTemporary(temporary.stage);
    }
    this.exec("DELETE FROM temp_objects");
    this.exec("UPDATE stages SET phase='failed' WHERE phase IN ('receiving','decoded') OR (phase='verified' AND retained=0)");
    for (;;) {
      const pending = this.exec("SELECT id FROM stages WHERE phase='failed' LIMIT 1")[0];
      if (!pending) return;
      await this.abandon(pending.id);
    }
  }
}

export class ChunkWriter {
  constructor(store, stage, temporary = null) {
    this.store = store; this.stage = stage; this.temporary = temporary;
    this.buffer = new Uint8Array(CHUNK_BYTES); this.used = 0; this.position = 0; this.seq = 0;
  }
  async write(bytes) {
    let offset = 0;
    while (offset < bytes.length) {
      const count = Math.min(bytes.length - offset, CHUNK_BYTES - this.used);
      this.buffer.set(bytes.subarray(offset, offset + count), this.used); this.used += count; offset += count;
      if (this.used === CHUNK_BYTES) await this.flush();
    }
  }
  async flush() {
    if (this.used === 0) return;
    const data = this.buffer.subarray(0, this.used);
    const sha = await sha256(data);
    const prefix = `repositories/${this.store.namespace}`;
    const key = this.temporary === null ? `${prefix}/chunks/${sha}` : `${prefix}/tmp/${this.stage}/${this.temporary}/${this.seq}`;
    const values = [this.stage, this.seq, key, this.position, this.used, sha];
    if (this.temporary === null) this.store.exec("INSERT INTO stage_chunks(stage,seq,key,start,length,sha) VALUES(?,?,?,?,?,?)", ...values);
    else this.store.exec("INSERT INTO temp_chunks(stage,object,seq,key,start,length,sha) VALUES(?,?,?,?,?,?,?)", this.stage, this.temporary, ...values.slice(1));
    await this.store.put(key, data);
    this.position += this.used; this.seq++; this.used = 0;
  }
  async finish() { await this.flush(); this.buffer = new Uint8Array(); return this.position; }
}
