// Durable storage for opaque codec records and authenticated byte chunks.
import { bucket } from "./r2-s3.mjs";
export const CHUNK_BYTES = 8 * 1024 * 1024;
export const PAGE_BYTES = 65536;

export function blob(bytes) { return bytes.slice().buffer; }
export function bytes(value) { return value instanceof Uint8Array ? value : new Uint8Array(value); }
export async function sha256(value) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", value));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export class ByteCache {
  constructor(limit, maxEntries = 8192) { this.limit = limit; this.maxEntries = maxEntries; this.size = 0; this.peak = 0; this.entries = new Map(); }
  get(key) {
    const value = this.entries.get(key);
    if (value === undefined) return undefined;
    this.entries.delete(key); this.entries.set(key, value); return value;
  }
  put(key, value) {
    if (!(value instanceof Uint8Array)) throw new TypeError("cache stores bytes");
    if (value.byteLength > this.limit || this.maxEntries === 0) return;
    const old = this.entries.get(key);
    if (old !== undefined) { this.entries.delete(key); this.size -= old.byteLength; }
    while (this.size + value.byteLength > this.limit || this.entries.size >= this.maxEntries) {
      const first = this.entries.keys().next().value;
      this.size -= this.entries.get(first).byteLength; this.entries.delete(first);
    }
    this.entries.set(key, value); this.size += value.byteLength; this.peak = Math.max(this.peak, this.size);
  }
}

const SCHEMA = [
  "CREATE TABLE IF NOT EXISTS host_identity (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL)",
  "CREATE TABLE IF NOT EXISTS repository (id INTEGER PRIMARY KEY, format INTEGER NOT NULL, generation INTEGER NOT NULL DEFAULT 0, object_count INTEGER NOT NULL DEFAULT 0, head BLOB NOT NULL)",
  "CREATE TABLE IF NOT EXISTS stages (id TEXT PRIMARY KEY, format INTEGER NOT NULL, phase TEXT NOT NULL, retained INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, pack TEXT, bytes INTEGER NOT NULL DEFAULT 0, selected INTEGER NOT NULL DEFAULT 0, expected INTEGER NOT NULL DEFAULT 0, reusable INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID",
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
  "CREATE TABLE IF NOT EXISTS stage_edges (stage TEXT NOT NULL, node INTEGER NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, relation INTEGER NOT NULL, PRIMARY KEY(stage,node,relation,dst)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_names (stage TEXT NOT NULL, node INTEGER NOT NULL, name BLOB NOT NULL, mode INTEGER NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, PRIMARY KEY(stage,node,name)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS temp_chunks (stage TEXT NOT NULL, object TEXT NOT NULL, seq INTEGER NOT NULL, key TEXT NOT NULL, start INTEGER NOT NULL, length INTEGER NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(stage,object,seq)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS temp_chunk_offsets ON temp_chunks(stage,object,start)",
  "CREATE TABLE IF NOT EXISTS temp_objects (stage TEXT NOT NULL, object TEXT NOT NULL, oid TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY(stage,object)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS refs (name BLOB PRIMARY KEY, oid TEXT NOT NULL, peeled TEXT) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS proposed_refs (name BLOB PRIMARY KEY, seq INTEGER) WITHOUT ROWID",
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
const STAGE_METADATA = ["stage_objects", "stage_edges", "stage_names", "stage_commands", "stage_options", "stage_shallow"];

export class Store {
  constructor(ctx, env, namespace) {
    this.ctx = ctx; this.env = env; this.namespace = namespace; this.blobs = bucket(env);
    this.usage = { sqlCalls: 0, sqlRowsRead: 0, sqlRowsWritten: 0, r2Gets: 0, r2Puts: 0, r2Deletes: 0, r2ReadBytes: 0, r2WrittenBytes: 0 };
    this.sqlKnown = true;
    for (const statement of SCHEMA) this.exec(statement);
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
    const cursor = this.ctx.storage.sql.exec(statement, ...bindings);
    const rows = cursor.toArray();
    this.usage.sqlCalls++;
    if (typeof cursor.rowsRead === "number" && typeof cursor.rowsWritten === "number") {
      this.usage.sqlRowsRead += cursor.rowsRead; this.usage.sqlRowsWritten += cursor.rowsWritten;
    } else this.sqlKnown = false;
    return rows;
  }
  initialize(format) {
    const current = this.exec("SELECT * FROM repository WHERE id=1")[0];
    if (current && current.format !== format) throw new Error("repository object format differs");
    if (!current) this.exec("INSERT INTO repository(id,format,generation,head) VALUES(1,?,0,?)", format, blob(new TextEncoder().encode("refs/heads/main")));
  }
  stage(format, retained = false) {
    this.initialize(format);
    const id = crypto.randomUUID();
    this.exec("INSERT INTO stages(id,format,phase,created,retained) VALUES(?,?,?,?,?)", id, format, "receiving", Date.now(), Number(retained));
    return id;
  }
  node(stage, node) { return this.exec("SELECT * FROM stage_objects WHERE stage=? AND node=?", stage, node)[0]; }
  object(stage, oid) {
    return this.exec("SELECT * FROM stage_objects WHERE stage=? AND oid=? AND verified=1 LIMIT 1", stage, oid)[0]
      ?? this.published(oid);
  }
  published(oid) {
    return this.exec("SELECT o.* FROM stage_objects o JOIN stages s ON s.id=o.stage WHERE o.oid=? AND s.phase='published' AND o.verified=1 ORDER BY o.depth,o.stage LIMIT 1", oid)[0];
  }
  peeled(oid, stage = null) {
    const find = (id) => stage ? this.object(stage, id) : this.published(id);
    let row = find(oid);
    for (let depth = 0; row?.kind === 4; depth++) {
      if (depth >= 128) throw new Error("tag peeling budget exceeded");
      const edge = this.exec("SELECT dst FROM stage_edges WHERE stage=? AND node=? AND relation=2 LIMIT 1", row.stage, row.node)[0];
      if (!edge) throw new Error("stored tag has no target");
      row = find(edge.dst);
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
      treeNameScratchRows: this.exec("SELECT COUNT(*) AS count FROM stage_names")[0].count,
      quarantineRows: STAGE_METADATA.reduce((count, table) => count + this.exec(`SELECT COUNT(*) AS count FROM ${table} d JOIN stages s ON s.id=d.stage WHERE s.phase='failed'`)[0].count, 0),
      refs: this.exec("SELECT COUNT(*) AS count FROM refs")[0].count,
      stages: this.exec("SELECT id,format,phase,bytes,expected FROM stages ORDER BY created DESC LIMIT 64"),
      databaseBytes: this.ctx.storage.sql.databaseSize ?? null,
      workerCpuMs: null,
    };
    result.usage = { ...this.usage, sqlRowsRead: this.sqlKnown ? this.usage.sqlRowsRead : null, sqlRowsWritten: this.sqlKnown ? this.usage.sqlRowsWritten : null };
    return result;
  }
  async put(key, data) {
    this.usage.r2Puts++; this.usage.r2WrittenBytes += data.byteLength;
    await this.blobs.put(key, data);
  }
  async get(chunk, cache) {
    const hit = cache.get(chunk.key);
    if (hit) return hit;
    this.usage.r2Gets++;
    const object = await this.blobs.get(chunk.key);
    if (!object || object.size !== chunk.length || object.size > CHUNK_BYTES) throw new Error("missing or invalid stored chunk");
    const data = new Uint8Array(await object.arrayBuffer());
    this.usage.r2ReadBytes += data.byteLength;
    if (await sha256(data) !== chunk.sha) throw new Error("stored chunk hash differs");
    cache.put(chunk.key, data); return data;
  }
  async clearTemporary(stage) {
    try {
    for (;;) {
      const rows = this.exec("SELECT object,seq,key FROM temp_chunks WHERE stage=? LIMIT 1000", stage);
      if (!rows.length) break;
      const batch = rows.map((row) => row.key);
      this.usage.r2Deletes += batch.length; await this.blobs.delete(batch);
      for (const row of rows) this.exec("DELETE FROM temp_chunks WHERE stage=? AND object=? AND seq=?", stage, row.object, row.seq);
    }
    this.exec("DELETE FROM temp_objects WHERE stage=?", stage);
    } catch (error) { await this.ctx.storage.setAlarm(Date.now() + 60000); throw error; }
  }
  async abandon(stage) {
    try {
    const current = this.exec("SELECT phase FROM stages WHERE id=?", stage)[0];
    if (!current) return;
    if (current.phase === "published") throw new Error("cannot abandon published storage");
    this.exec("UPDATE stages SET phase='failed' WHERE id=?", stage);
      await this.clearTemporary(stage);
      for (;;) {
        const rows = this.exec("SELECT seq,key FROM stage_chunks WHERE stage=? LIMIT 128", stage);
        if (!rows.length) break;
        for (const row of rows) {
          // Claims precede R2 puts. Delete the last owner's bytes before its
          // claim, so a crash/outage always leaves a durable retry record.
          if (!this.exec("SELECT 1 FROM stage_chunks WHERE key=? AND stage<>? LIMIT 1", row.key, stage).length) {
            this.usage.r2Deletes++; await this.blobs.delete(row.key);
          }
          this.exec("DELETE FROM stage_chunks WHERE stage=? AND seq=?", stage, row.seq);
        }
      }
      for (const table of STAGE_METADATA) this.exec(`DELETE FROM ${table} WHERE stage=?`, stage);
      this.exec("DELETE FROM stages WHERE id=? AND phase='failed'", stage);
    } catch (error) {
      await this.ctx.storage.setAlarm(Date.now() + 60000);
      throw error;
    }
  }
  async recover() {
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
