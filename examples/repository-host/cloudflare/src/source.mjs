import { hex, integer } from "./codec.mjs";
import { ByteCache, ChunkWriter, PAGE_BYTES } from "./storage.mjs";

const CANONICAL_ENTRY_BYTES = 8 * 1024 * 1024;
const REPRESENTATION_ENTRY_BYTES = 1024 * 1024;
const rowKey = (row) => row.stage ? `s:${row.stage}:${row.node}` : `o:${row.oid}`;
const info = (row) => ({ kind: row.kind, size: row.size });

// A request-local storage adapter. Canonical views are changed only between
// reconstruction queries. Keeping verified bases resolved avoids repeatedly
// interpreting the same deep delta chain for each requested output fragment.
export class ObjectSource {
  constructor(store, codec, stage, namespace) {
    this.store = store; this.codec = codec; this.stage = stage; this.namespace = namespace;
    this.canonical = new ByteCache(24 * 1024 * 1024);
    this.representations = new ByteCache(8 * 1024 * 1024);
    this.chunks = new ByteCache(16 * 1024 * 1024);
    this.reader = null;
    this.temporary = new Map();
    this.queries = 0; this.storageReads = 0; this.historicalModes = 0;
  }

  key(row) { return row.stage ? { staged: row.node } : { id: Uint8Array.from(row.oid.match(/../g), (pair) => Number.parseInt(pair, 16)) }; }
  lookup(key) {
    const row = key.staged !== undefined ? this.store.node(this.stage, integer(key.staged)) : this.store.object(this.stage, hex(key.id));
    if (!row || !row.verified || row.kind === null || row.size === null) throw new Error("canonical base is absent or unverified");
    return row;
  }
  base(row) {
    if (row.base_node !== null && row.base_node !== undefined && row.stage) return this.store.node(row.stage, row.base_node);
    if (row.base_oid) return this.store.object(this.stage, row.base_oid);
    if (row.wire_base_offset !== null && row.wire_base_offset !== undefined) return this.store.node(row.stage, row.wire_base_offset);
    if (row.wire_base_id) return this.store.object(this.stage, row.wire_base_id);
    return undefined;
  }
  cachedTemporary(row) {
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
      this.reader = { key, next: offset, wire: row.node, input: new Uint8Array(), cursor: 0 };
    }
    const result = new Uint8Array(length);
    let written = 0;
    while (written < length) {
      const reader = this.reader;
      if (reader.cursor === reader.input.length) {
        const bytes = await this.chunkAt(row, reader.wire);
        reader.input = bytes.subarray(0, Math.min(bytes.length, row.end_offset - reader.wire, PAGE_BYTES));
        reader.cursor = 0;
      }
      const input = reader.input.subarray(reader.cursor);
      const event = this.codec.sourceStep(input, reader.wire + input.length === row.end_offset, Math.min(length - written, PAGE_BYTES));
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

  scan(row, data, eof, scan) {
    let consumed = 0;
    for (;;) {
      const event = this.codec.graphFeed(data.subarray(consumed), eof);
      consumed += event.consumed;
      if (scan && event.tag === 31) this.store.exec("INSERT OR IGNORE INTO stage_edges(stage,node,dst,kind,relation) VALUES(?,?,?,?,?)", this.stage, row.node, hex(event.id), event.flags, integer(event.a));
      if (scan && event.tag === 32) {
        const kind = event.flags & 255;
        // A database uniqueness constraint checks aliases even when they are
        // separated in Git's tree order. Raw names are bound as exact BLOBs.
        this.store.exec("INSERT INTO stage_names(stage,node,name,mode,dst,kind) VALUES(?,?,?,?,?,?)", this.stage, row.node, event.data.slice().buffer, integer(event.a), hex(event.id), kind);
        if (kind) this.store.exec("INSERT OR IGNORE INTO stage_edges(stage,node,dst,kind,relation) VALUES(?,?,?,?,?)", this.stage, row.node, hex(event.id), kind, 3);
        if (!(event.flags & 256)) this.historicalModes++;
      }
      if (event.tag === 33) {
        if (consumed !== data.length) throw new Error("graph ended with unconsumed body bytes");
        return hex(event.id);
      }
      if (event.tag === 30 && consumed === data.length && !eof) return undefined;
    }
  }

  async process(row, scan = true) {
    this.queries++;
    const descriptor = this.descriptor(row);
    this.codec.rangeInit(this.key(row)); this.codec.graphInit(info(row));
    const captured = row.size <= CANONICAL_ENTRY_BYTES ? new Uint8Array(row.size) : null;
    const temporary = captured === null ? new ChunkWriter(this.store, this.stage, rowKey(row)) : null;
    let written = 0, digest;
    for (;;) {
      const request = this.codec.rangeRequest();
      if (request.tag === 20) {
        // The root may be quarantined; every descended base must be verified.
        const current = request.key.staged !== undefined && integer(request.key.staged) === row.node && row.stage === this.stage ? row : this.lookup(request.key);
        this.codec.rangeDescribe(current === row ? descriptor : this.descriptor(current));
      } else if (request.tag === 21) {
        const root = request.key.staged !== undefined && integer(request.key.staged) === row.node && row.stage === this.stage;
        const offset = integer(request.a), length = integer(request.b);
        const input = root ? await this.readRow(row, request.flags, offset, length) : await this.read(request.key, request.flags, offset, length);
        const event = this.codec.rangeFeed(input);
        if (captured) captured.set(event.data, written);
        if (temporary && event.data.length) await temporary.write(event.data);
        written += event.written;
        digest ??= this.scan(row, event.data, event.tag === 22, scan);
      } else if (request.tag === 22) {
        digest ??= this.scan(row, new Uint8Array(), true, scan);
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
    if (scan) this.store.exec("UPDATE stage_objects SET oid=?,verified=1,scanned=1 WHERE stage=? AND node=?", digest, this.stage, row.node);
    if (captured) this.canonical.put(digest, captured);
    return digest;
  }
}
