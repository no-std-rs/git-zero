import { hex, integer } from "./codec.mjs";
import { ByteCache, CacheBuffers, ChunkWriter, PAGE_BYTES } from "./storage.mjs";

const CANONICAL_ENTRY_BYTES = 8 * 1024 * 1024;
const REPRESENTATION_ENTRY_BYTES = 1024 * 1024;
// At most 96 bindings for names, 80 for edges; never buffer a whole tree.
const METADATA_BATCH = 16;
const rowKey = (row) => row.stage ? `s:${row.stage}:${row.node}` : `o:${row.oid}`;
const info = (row) => ({ kind: row.kind, size: row.size });

// A request-local storage adapter. Canonical views are changed only between
// reconstruction queries. Keeping verified bases resolved avoids repeatedly
// interpreting the same deep delta chain for each requested output fragment.
export class ObjectSource {
  constructor(store, codec, stage, { ownedBuffers = false } = {}) {
    this.store = store; this.codec = codec; this.stage = stage; this.ownsBuffers = ownedBuffers;
    // Owned views cannot escape beyond the next cache mutation. Intake consumes
    // them synchronously; fetch copies them into codec frames before yielding.
    const ownership = ownedBuffers ? new CacheBuffers() : null;
    this.canonical = new ByteCache(24 * 1024 * 1024, 8192, ownership);
    this.representations = new ByteCache(8 * 1024 * 1024, 8192, ownership);
    this.chunks = new ByteCache(16 * 1024 * 1024, 8192, ownership);
    this.reader = null; this.readBuffer = null;
    this.temporary = new Map();
    this.bases = new Map();
    this.names = []; this.edges = []; this.graphStage = null;
    this.queries = 0; this.storageReads = 0; this.historicalModes = 0;
  }

  releaseCaches() {
    this.readBuffer?.buffer.transfer(0);
    this.reader = null; this.readBuffer = null;
    this.bases.clear();
    for (const cache of [this.canonical, this.representations, this.chunks]) cache.clear();
  }

  key(row) {
    const physical = rowKey(row);
    let index = this.queryKeys.get(physical);
    if (index === undefined) {
      if (this.queryRows.length > this.codec.limits.maxDepth) throw new Error("query metadata budget exceeded");
      index = this.queryRows.length; this.queryRows.push(row); this.queryKeys.set(physical, index);
    }
    return { staged: index };
  }
  lookup(key) {
    const row = key.staged !== undefined ? this.queryRows[integer(key.staged)] : undefined;
    if (!row || !row.verified || row.kind === null || row.size === null) throw new Error("canonical base is absent or unverified");
    return row;
  }
  base(row) {
    if (row.base_stage && row.base_node !== null) return this.loadBase(row.base_stage, row.base_node);
    if (row.base_node !== null && row.base_node !== undefined && row.stage) return this.loadBase(row.stage, row.base_node);
    if (row.base_oid) return this.loadBase(null, null, row.base_oid);
    if (row.wire_base_offset !== null && row.wire_base_offset !== undefined) return this.loadBase(row.stage, row.wire_base_offset);
    if (row.wire_base_id) return this.loadBase(null, null, row.wire_base_id);
    return undefined;
  }
  loadBase(stage, node, oid = null) {
    const key = oid === null ? `s:${stage}:${node}` : `o:${oid}`;
    const cached = this.bases.get(key);
    if (cached) return cached;
    const row = oid === null ? this.store.node(stage, node) : this.store.object(this.stage, oid);
    // Only authenticated, immutable rows survive a lookup. A missing or
    // unresolved base must remain visible when validation advances.
    if (row?.verified) {
      for (const alias of [key, rowKey(row)]) {
        if (!this.bases.has(alias) && this.bases.size >= 256) this.bases.delete(this.bases.keys().next().value);
        this.bases.set(alias, row);
      }
    }
    return row;
  }
  cachedTemporary(row) {
    // process() captures these bodies; only larger objects create temp rows.
    if (row.size <= CANONICAL_ENTRY_BYTES) return undefined;
    const key = rowKey(row);
    if (this.temporary.has(key)) return this.temporary.get(key);
    const value = this.store.exec("SELECT * FROM temp_objects WHERE stage=? AND object=?", this.stage, key)[0];
    if (value) {
      if (value.oid !== row.oid || value.size !== row.size) throw new Error("temporary canonical identity differs");
      this.rememberTemporary(key, value);
    }
    return value;
  }
  rememberTemporary(key, value) {
    if (!this.temporary.has(key) && this.temporary.size >= 256) this.temporary.delete(this.temporary.keys().next().value);
    this.temporary.set(key, value);
  }
  seed(row, bytes) {
    if (bytes.length !== row.packed_size) throw new Error("representation cache length differs");
    this.representations.put(rowKey(row), bytes);
  }
  descriptor(row, nesting = 0) {
    if (nesting > this.codec.limits.maxDepth) throw new Error("metadata recursion budget exceeded");
    if (this.canonical.get(row.oid) || this.cachedTemporary(row) || row.wire_kind < 5) return { info: info(row) };
    const base = this.base(row);
    if (!base?.verified) throw new Error("delta base is absent or unverified");
    const view = this.descriptor(base, nesting + 1);
    return { info: info(row), base: { key: this.key(base), info: info(base), depth: view.base === undefined ? 0 : view.base.depth + 1 }, instructionsSize: row.packed_size };
  }

  async chunkAt(row, offset, temporary = false) {
    let chunk;
    if (temporary) chunk = this.store.exec("SELECT * FROM temp_chunks WHERE stage=? AND object=? AND start<=? ORDER BY start DESC LIMIT 1", this.stage, rowKey(row), offset)[0];
    else if (row.stage) chunk = this.store.exec("SELECT * FROM stage_chunks WHERE stage=? AND start<=? ORDER BY start DESC LIMIT 1", row.stage, offset)[0];
    else chunk = this.store.exec("SELECT * FROM chunks WHERE pack=? AND start<=? ORDER BY start DESC LIMIT 1", row.pack, offset)[0];
    if (!chunk || offset >= chunk.start + chunk.length) throw new Error("encoded storage range is absent");
    const data = await this.store.get(chunk, this.chunks);
    return data.subarray(offset - chunk.start);
  }

  async encoded(row, offset, length) {
    const key = rowKey(row);
    if (this.reader?.key !== key || this.reader.next !== offset) {
      this.codec.sourceInit({ offset: row.node, maxBytes: row.end_offset - row.node }, { offset, length: row.packed_size - offset });
      this.reader = { key, next: offset, wire: row.node, inflated: 0, input: new Uint8Array(), cursor: 0 };
    }
    const result = new Uint8Array(length);
    let written = 0;
    while (written < length) {
      const reader = this.reader;
      if (reader.cursor === reader.input.length) {
        const bytes = await this.chunkAt(row, reader.wire);
        reader.input = bytes.subarray(0, Math.min(bytes.length, row.end_offset - reader.wire, PAGE_BYTES));
        // The inflater can resume this page after other chunk-cache reads.
        if (this.ownsBuffers) {
          this.readBuffer ??= new Uint8Array(PAGE_BYTES);
          this.readBuffer.set(reader.input);
          reader.input = this.readBuffer.subarray(0, reader.input.length);
        }
        reader.cursor = 0;
      }
      const input = reader.input.subarray(reader.cursor);
      const event = this.codec.sourceStep(input, reader.wire + input.length === row.end_offset, Math.min(length - written, PAGE_BYTES));
      const inflated = integer(event.b);
      this.codec.charge(1, inflated - reader.inflated); reader.inflated = inflated;
      reader.cursor += event.consumed; reader.wire += event.consumed;
      result.set(event.data, written); written += event.written; reader.next += event.written;
      if (event.tag === 41 && written !== length) throw new Error("source ended before requested range");
      if (event.consumed === 0 && event.written === 0 && event.tag === 40 && event.flags === 1 && reader.wire === row.end_offset) throw new Error("truncated stored entry");
    }
    return result;
  }

  async representation(row, offset, length) {
    this.storageReads++;
    const key = rowKey(row);
    const full = this.representations.get(key);
    if (full) return full.subarray(offset, Math.min(offset + length, full.length));
    if (row.packed_size <= REPRESENTATION_ENTRY_BYTES) {
      const bytes = await this.encoded(row, 0, row.packed_size);
      this.representations.put(key, bytes);
      return bytes.subarray(offset, Math.min(offset + length, bytes.length));
    }
    const start = Math.floor(offset / PAGE_BYTES) * PAGE_BYTES;
    const pageKey = `${key}:${start}`;
    let page = this.representations.get(pageKey);
    if (!page) {
      page = await this.encoded(row, start, Math.min(PAGE_BYTES, row.packed_size - start));
      this.representations.put(pageKey, page);
    }
    return page.subarray(offset - start, Math.min(offset - start + length, page.length));
  }

  async read(key, kind, offset, length) {
    const row = this.lookup(key);
    return this.readRow(row, kind, offset, length);
  }

  async readRow(row, kind, offset, length) {
    if (kind === 0) {
      const cached = this.canonical.get(row.oid);
      if (cached) return cached.subarray(offset, Math.min(offset + length, cached.length));
      if (this.cachedTemporary(row)) {
        const chunk = await this.chunkAt(row, offset, true);
        return chunk.subarray(0, Math.min(length, chunk.length));
      }
    }
    return this.representation(row, offset, length);
  }

  async warm(row, nesting = 0) {
    if (!row?.verified) throw new Error("unverified base cannot be warmed");
    if (nesting > this.codec.limits.maxDepth) throw new Error("base depth budget exceeded");
    if (this.canonical.get(row.oid) || this.cachedTemporary(row)) return;
    if (row.wire_kind >= 5) await this.warm(this.base(row), nesting + 1);
    await this.process(row, false);
  }

  flushMetadata() {
    if (this.names.length) {
      this.store.exec(`INSERT INTO stage_names(stage,node,name,mode,dst,kind) VALUES${Array(this.names.length).fill("(?,?,?,?,?,?)").join(",")}`, ...this.names.flat());
      this.names.length = 0;
    }
    if (this.edges.length) {
      this.store.exec(`INSERT OR IGNORE INTO stage_edges(stage,node,dst,kind,relation) VALUES${Array(this.edges.length).fill("(?,?,?,?,?)").join(",")}`, ...this.edges.flat());
      this.edges.length = 0;
    }
  }

  edge(row, id, kind, relation) {
    this.edges.push([this.graphStage ??= this.store.edgeStage(this.stage), row.node, this.store.edgeTarget(id), kind, relation]);
    if (this.edges.length === METADATA_BATCH) this.flushMetadata();
  }

  beginScan(row, strict = true, checks = 0) {
    this.namesStaged = false;
    this.names.length = 0; this.edges.length = 0;
    this.codec.graphInit(info(row), { strict, checks, receivedPlain: Boolean(checks & 1) && row.stage === this.stage && row.wire_kind === 3 });
  }

  finishScan(row) {
    // Names establish uniqueness within one authenticated tree. Immutable tree
    // bytes and typed edges are sufficient once its full digest is verified.
    if (this.namesStaged) this.store.exec("DELETE FROM stage_names WHERE stage=? AND node=?", this.stage, row.node);
    this.namesStaged = false;
  }

  scan(row, data, eof, scan) {
    let consumed = 0;
    for (;;) {
      const event = this.codec.graphFeed(data.subarray(consumed), eof);
      consumed += event.consumed;
      if (scan && event.tag === 31) this.edge(row, hex(event.id), event.flags, integer(event.a));
      if (scan && event.tag === 32) {
        const checks = event.flags >>> 10 & 3;
        if (checks) this.store.exec("INSERT INTO stage_blob_checks(stage,oid,checks) VALUES(?,?,?) ON CONFLICT(stage,oid) DO UPDATE SET checks=stage_blob_checks.checks|excluded.checks WHERE (stage_blob_checks.checks&excluded.checks)<>excluded.checks", this.stage, hex(event.id), checks);
        this.namesStaged = true;
        const kind = event.flags & 255;
        // A database uniqueness constraint checks aliases even when they are
        // separated in Git's tree order. Raw names are bound as exact BLOBs.
        const id = hex(event.id);
        this.names.push([this.stage, row.node, event.data.slice().buffer, integer(event.a), id, kind]);
        if (this.names.length === METADATA_BATCH) this.flushMetadata();
        if (kind) this.edge(row, id, kind, 3);
        if (!(event.flags & 256)) this.historicalModes++;
      }
      if (event.tag === 33) {
        if (consumed !== data.length) throw new Error("graph ended with unconsumed body bytes");
        if (scan) this.flushMetadata();
        return hex(event.id);
      }
      if (event.tag === 30 && consumed === data.length && !eof) return undefined;
    }
  }

  scanCaptured(row, data, scan) {
    let cursor = 0, digest;
    do {
      const end = Math.min(cursor + PAGE_BYTES, data.length);
      digest = this.scan(row, data.subarray(cursor, end), end === data.length, scan);
      cursor = end;
    } while (cursor < data.length);
    return digest;
  }

  async process(row, scan = true, checks = 0) {
    this.queries++;
    this.codec.charge(0, row.size);
    // Designated policies may rescan a body reconstructed earlier in this
    // request. Reuse its authenticated temporary object and reserve it once.
    const createTemporary = row.size > CANONICAL_ENTRY_BYTES && !this.cachedTemporary(row);
    if (createTemporary) this.codec.charge(2, row.size);
    // Keys identify physical immutable representations within one query. An
    // identity delta may have the same canonical ID as its external base;
    // resolving by ID again would accidentally choose the delta itself.
    this.queryKeys = new Map(); this.queryRows = [];
    const rootKey = this.key(row);
    const descriptor = this.descriptor(row);
    this.codec.rangeInit(rootKey); this.beginScan(row, scan, checks);
    const captured = row.size <= CANONICAL_ENTRY_BYTES ? new Uint8Array(row.size) : null;
    const temporary = createTemporary ? new ChunkWriter(this.store, this.stage, rowKey(row)) : null;
    let written = 0, digest, request = this.codec.rangeRequest();
    for (;;) {
      if (request.tag === 20) {
        // The root may be quarantined; every descended base must be verified.
        const current = request.key.staged !== undefined && integer(request.key.staged) === rootKey.staged ? row : this.lookup(request.key);
        this.codec.rangeDescribe(current === row ? descriptor : this.descriptor(current));
        request = this.codec.rangeRequest();
      } else if (request.tag === 21) {
        const root = request.key.staged !== undefined && integer(request.key.staged) === rootKey.staged;
        const offset = integer(request.a), length = integer(request.b);
        const input = root ? await this.readRow(row, request.flags, offset, length) : await this.read(request.key, request.flags, offset, length);
        const event = this.codec.rangeFeed(input);
        this.codec.charge(1, event.consumed);
        if (captured) captured.set(event.data, written);
        if (temporary && event.data.length) await temporary.write(event.data);
        written += event.written;
        if (!captured && (event.data.length || event.tag === 22)) digest ??= this.scan(row, event.data, event.tag === 22, scan);
        request = event;
      } else if (request.tag === 22) {
        // Captured bytes are already bounded and owned by this query. Scan
        // whole pages instead of crossing the ABI for each tiny delta copy.
        if (captured) {
          digest = this.scanCaptured(row, captured, scan);
        } else digest ??= this.scan(row, new Uint8Array(), true, scan);
        break;
      } else throw new Error("unexpected reconstruction event");
    }
    if (written !== row.size || !digest || (row.oid && row.oid !== digest)) throw new Error("canonical object hash differs");
    if (temporary) {
      const length = await temporary.finish();
      if (length !== row.size) throw new Error("temporary body length differs");
      this.store.exec("INSERT OR REPLACE INTO temp_objects(stage,object,oid,size) VALUES(?,?,?,?)", this.stage, rowKey(row), digest, row.size);
      this.rememberTemporary(rowKey(row), { oid: digest, size: row.size });
    }
    if (scan) {
      // Publish derived metadata to the private index only with its verified
      // digest. One update avoids rewriting the global OID/depth index twice.
      this.store.exec("UPDATE stage_objects SET oid=?,kind=?,size=?,depth=?,base_stage=?,base_node=?,base_oid=?,verified=1,scanned=1 WHERE stage=? AND node=?", digest, row.kind, row.size, row.depth, row.base_stage, row.base_node, row.base_oid, this.stage, row.node);
      this.finishScan(row);
    }
    if (captured) this.canonical.put(digest, captured);
    return digest;
  }
}
