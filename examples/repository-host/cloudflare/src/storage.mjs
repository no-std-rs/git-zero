// Durable storage for opaque codec records and authenticated byte chunks.
export const CHUNK_BYTES = 8 * 1024 * 1024;
export const PAGE_BYTES = 65536;

export function blob(bytes) { return bytes.slice().buffer; }
export function bytes(value) { return value instanceof Uint8Array ? value : new Uint8Array(value); }
export async function sha256(value) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", value));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export class ByteCache {
  constructor(limit) { this.limit = limit; this.size = 0; this.peak = 0; this.entries = new Map(); }
  get(key) {
    const value = this.entries.get(key);
    if (value === undefined) return undefined;
    this.entries.delete(key); this.entries.set(key, value); return value;
  }
  put(key, value) {
    if (!(value instanceof Uint8Array)) throw new TypeError("cache stores bytes");
    if (value.byteLength > this.limit) return;
    const old = this.entries.get(key);
    if (old !== undefined) { this.entries.delete(key); this.size -= old.byteLength; }
    while (this.size + value.byteLength > this.limit) {
      const first = this.entries.keys().next().value;
      this.size -= this.entries.get(first).byteLength; this.entries.delete(first);
    }
    this.entries.set(key, value); this.size += value.byteLength; this.peak = Math.max(this.peak, this.size);
  }
}

const SCHEMA = [
  "CREATE TABLE IF NOT EXISTS repository (id INTEGER PRIMARY KEY, format INTEGER NOT NULL, generation INTEGER NOT NULL DEFAULT 0, head BLOB NOT NULL)",
  "CREATE TABLE IF NOT EXISTS stages (id TEXT PRIMARY KEY, format INTEGER NOT NULL, phase TEXT NOT NULL, created INTEGER NOT NULL, pack TEXT, bytes INTEGER NOT NULL DEFAULT 0, selected INTEGER NOT NULL DEFAULT 0, expected INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_commands (stage TEXT NOT NULL, seq INTEGER NOT NULL, name BLOB NOT NULL, old_oid TEXT NOT NULL, new_oid TEXT NOT NULL, branch INTEGER NOT NULL, PRIMARY KEY(stage,seq), UNIQUE(stage,name)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_options (stage TEXT NOT NULL, seq INTEGER NOT NULL, value BLOB NOT NULL, PRIMARY KEY(stage,seq)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_shallow (stage TEXT NOT NULL, oid TEXT NOT NULL, PRIMARY KEY(stage,oid)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_chunks (stage TEXT NOT NULL, seq INTEGER NOT NULL, key TEXT NOT NULL, start INTEGER NOT NULL, length INTEGER NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(stage,seq)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS stage_chunk_offsets ON stage_chunks(stage,start)",
  "CREATE TABLE IF NOT EXISTS stage_objects (stage TEXT NOT NULL, node INTEGER NOT NULL, wire_kind INTEGER NOT NULL, packed_size INTEGER NOT NULL, compressed_offset INTEGER NOT NULL, end_offset INTEGER, crc INTEGER, wire_base_offset INTEGER, wire_base_id TEXT, oid TEXT, kind INTEGER, size INTEGER, depth INTEGER, base_node INTEGER, base_oid TEXT, verified INTEGER NOT NULL DEFAULT 0, scanned INTEGER NOT NULL DEFAULT 0, ready INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(stage,node)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS stage_object_ids ON stage_objects(stage,oid)",
  "CREATE INDEX IF NOT EXISTS stage_object_work ON stage_objects(stage,verified,scanned,node)",
  "CREATE INDEX IF NOT EXISTS stage_ready_work ON stage_objects(stage,verified,ready,node)",
  "CREATE INDEX IF NOT EXISTS stage_offset_dependencies ON stage_objects(stage,wire_base_offset,verified)",
  "CREATE INDEX IF NOT EXISTS stage_id_dependencies ON stage_objects(stage,wire_base_id,verified)",
  "CREATE TABLE IF NOT EXISTS stage_edges (stage TEXT NOT NULL, node INTEGER NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, relation INTEGER NOT NULL, PRIMARY KEY(stage,node,relation,dst)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS stage_names (stage TEXT NOT NULL, node INTEGER NOT NULL, name BLOB NOT NULL, mode INTEGER NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, PRIMARY KEY(stage,node,name)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS temp_chunks (stage TEXT NOT NULL, object TEXT NOT NULL, seq INTEGER NOT NULL, key TEXT NOT NULL, start INTEGER NOT NULL, length INTEGER NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(stage,object,seq)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS temp_chunk_offsets ON temp_chunks(stage,object,start)",
  "CREATE TABLE IF NOT EXISTS temp_objects (stage TEXT NOT NULL, object TEXT NOT NULL, oid TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY(stage,object)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS objects (oid TEXT PRIMARY KEY, kind INTEGER NOT NULL, size INTEGER NOT NULL, depth INTEGER NOT NULL, pack TEXT NOT NULL, offset INTEGER NOT NULL, end_offset INTEGER NOT NULL, compressed_offset INTEGER NOT NULL, wire_kind INTEGER NOT NULL, packed_size INTEGER NOT NULL, base_oid TEXT) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS packs (id TEXT PRIMARY KEY, bytes INTEGER NOT NULL, format INTEGER NOT NULL) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS chunks (pack TEXT NOT NULL, seq INTEGER NOT NULL, key TEXT NOT NULL, start INTEGER NOT NULL, length INTEGER NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(pack,seq)) WITHOUT ROWID",
  "CREATE INDEX IF NOT EXISTS chunk_offsets ON chunks(pack,start)",
  "CREATE TABLE IF NOT EXISTS refs (name BLOB PRIMARY KEY, oid TEXT NOT NULL) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS edges (oid TEXT NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, relation INTEGER NOT NULL, PRIMARY KEY(oid,relation,dst)) WITHOUT ROWID",
  "CREATE TABLE IF NOT EXISTS tree_names (oid TEXT NOT NULL, name BLOB NOT NULL, mode INTEGER NOT NULL, dst TEXT NOT NULL, kind INTEGER NOT NULL, PRIMARY KEY(oid,name)) WITHOUT ROWID",
];

export class Store {
  constructor(ctx, env, namespace) {
    this.ctx = ctx; this.env = env; this.namespace = namespace;
    this.usage = { sqlCalls: 0, sqlRowsRead: 0, sqlRowsWritten: 0, r2Gets: 0, r2Puts: 0, r2Deletes: 0, r2ReadBytes: 0, r2WrittenBytes: 0 };
    this.sqlKnown = true;
    for (const statement of SCHEMA) this.exec(statement);
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
  stage(format) {
    this.initialize(format);
    const id = crypto.randomUUID();
    this.exec("INSERT INTO stages(id,format,phase,created) VALUES(?,?,?,?)", id, format, "receiving", Date.now());
    return id;
  }
  node(stage, node) { return this.exec("SELECT * FROM stage_objects WHERE stage=? AND node=?", stage, node)[0]; }
  object(stage, oid) {
    return this.exec("SELECT * FROM stage_objects WHERE stage=? AND oid=? AND verified=1 LIMIT 1", stage, oid)[0]
      ?? this.exec("SELECT *,NULL AS stage,offset AS node,1 AS verified,1 AS scanned FROM objects WHERE oid=?", oid)[0];
  }
  statistics() {
    return {
      usage: { ...this.usage, sqlRowsRead: this.sqlKnown ? this.usage.sqlRowsRead : null, sqlRowsWritten: this.sqlKnown ? this.usage.sqlRowsWritten : null },
      objects: this.exec("SELECT COUNT(*) AS count FROM objects")[0].count,
      stagedObjects: this.exec("SELECT COUNT(*) AS count FROM stage_objects")[0].count,
      refs: this.exec("SELECT COUNT(*) AS count FROM refs")[0].count,
      stages: this.exec("SELECT id,format,phase,bytes,expected FROM stages ORDER BY created DESC LIMIT 64"),
      databaseBytes: this.ctx.storage.sql.databaseSize ?? null,
      workerCpuMs: null,
    };
  }
  async put(key, data) {
    this.usage.r2Puts++; this.usage.r2WrittenBytes += data.byteLength;
    await this.env.BLOBS.put(key, data);
  }
  async get(chunk, cache) {
    const hit = cache.get(chunk.key);
    if (hit) return hit;
    this.usage.r2Gets++;
    const object = await this.env.BLOBS.get(chunk.key);
    if (!object || object.size !== chunk.length || object.size > CHUNK_BYTES) throw new Error("missing or invalid stored chunk");
    const data = new Uint8Array(await object.arrayBuffer());
    this.usage.r2ReadBytes += data.byteLength;
    if (await sha256(data) !== chunk.sha) throw new Error("stored chunk hash differs");
    cache.put(chunk.key, data); return data;
  }
  async clearTemporary(stage) {
    const keys = this.exec("SELECT key FROM temp_chunks WHERE stage=?", stage).map((row) => row.key);
    for (let offset = 0; offset < keys.length; offset += 1000) {
      const batch = keys.slice(offset, offset + 1000);
      this.usage.r2Deletes += batch.length; await this.env.BLOBS.delete(batch);
    }
    this.exec("DELETE FROM temp_chunks WHERE stage=?", stage);
    this.exec("DELETE FROM temp_objects WHERE stage=?", stage);
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
    await this.store.put(key, data);
    const values = [this.stage, this.seq, key, this.position, this.used, sha];
    if (this.temporary === null) this.store.exec("INSERT INTO stage_chunks(stage,seq,key,start,length,sha) VALUES(?,?,?,?,?,?)", ...values);
    else this.store.exec("INSERT INTO temp_chunks(stage,object,seq,key,start,length,sha) VALUES(?,?,?,?,?,?,?)", this.stage, this.temporary, ...values.slice(1));
    this.position += this.used; this.seq++; this.used = 0;
  }
  async finish() { await this.flush(); this.buffer = new Uint8Array(); return this.position; }
}
