import { hex, integer } from "./codec.mjs";
import { ChunkWriter, blob } from "./storage.mjs";
import { ObjectSource } from "./source.mjs";

const EMPTY = new Uint8Array();
const REPRESENTATION_LIMIT = 1024 * 1024;
// Four complete records use 60 bindings, below the SQLite binding limit.
const OBJECT_COLUMNS = "stage,node,wire_kind,packed_size,compressed_offset,wire_base_offset,wire_base_id,kind,size,depth,end_offset,crc,oid,verified,scanned";
const OBJECT_VALUES = "(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)";

export class Intake {
  constructor(store, codec, stage) {
    this.store = store; this.codec = codec; this.stage = stage;
    this.source = new ObjectSource(store, codec, stage, store.namespace);
    this.writer = null; this.inPack = false; this.pack = null;
    this.commands = 0; this.options = 0; this.objects = 0; this.expected = 0;
    this.pendingObjects = [];
    this.current = null; this.lastEnd = 12; this.eof = false;
  }
  flushObjects() {
    if (!this.pendingObjects.length) return;
    this.store.exec(`INSERT INTO stage_objects(${OBJECT_COLUMNS}) VALUES${Array(this.pendingObjects.length).fill(OBJECT_VALUES).join(",")}`, ...this.pendingObjects.flat());
    this.pendingObjects.length = 0;
  }
  async event(event, input) {
    if (this.inPack && event.consumed) await this.writer.write(input.subarray(0, event.consumed));
    if ((event.tag === 9 || event.tag === 10) && event.written) {
      if (!this.current) throw new Error("body has no staged object");
      if (this.current.body) this.current.body.set(event.data, this.current.written);
      this.current.written += event.written;
    }
    const stage = this.stage;
    switch (event.tag) {
      case 2:
        this.store.exec("INSERT OR IGNORE INTO stage_shallow(stage,oid) VALUES(?,?)", stage, hex(event.id)); break;
      case 3:
        this.store.exec("INSERT INTO stage_commands(stage,seq,name,old_oid,new_oid,branch,creation,deletion) VALUES(?,?,?,?,?,?,?,?)", stage, this.commands++, blob(event.data), hex(event.id), hex(event.other), event.flags & 1, (event.flags >> 1) & 1, (event.flags >> 2) & 1); break;
      case 5:
        this.store.exec("INSERT INTO stage_options(stage,seq,value) VALUES(?,?,?)", stage, this.options++, blob(event.data)); break;
      case 6:
        this.inPack = !!event.flags;
        this.store.exec("UPDATE stages SET selected=? WHERE id=?", this.codec.receiveCapabilities(), stage);
        if (this.inPack) this.writer = new ChunkWriter(this.store, stage);
        break;
      case 7:
        this.expected = integer(event.a); break;
      case 8: {
        const node = integer(event.a), size = integer(event.b), kind = event.flags;
        this.current = { node, size, kind, written: 0, body: size <= REPRESENTATION_LIMIT ? new Uint8Array(size) : null, fields: [stage, node, kind, size, integer(event.c), kind === 6 ? integer(event.d) : null, kind === 7 ? hex(event.id) : null, kind < 5 ? kind : null, kind < 5 ? size : null, kind < 5 ? 0 : null] };
        this.objects++; break;
      }
      case 10: {
        const current = this.current;
        if (!current || current.written !== current.size || !!event.flags !== (current.kind < 5)) throw new Error("staged entry metadata differs");
        this.lastEnd = integer(event.a);
        this.pendingObjects.push([...current.fields, this.lastEnd, integer(event.b), event.flags ? hex(event.id) : null, event.flags ? 1 : 0, event.flags & 2 ? 1 : 0]);
        if (this.pendingObjects.length === 4) this.flushObjects();
        if (current.body) {
          this.source.seed({ stage, node: current.node, packed_size: current.size }, current.body);
          if (event.flags & 2) this.source.canonical.put(hex(event.id), current.body);
        }
        this.current = null; break;
      }
      case 11:
        this.pack = hex(event.id); break;
      case 12:
        if (integer(event.a) !== this.commands) throw new Error("command count differs");
        this.eof = true; this.inPack = false; break;
    }
  }

  async decode(request) {
    this.codec.receiveInit();
    const reader = request.body?.getReader();
    if (reader) {
      try {
        for (;;) {
          const item = await reader.read();
          if (item.done) break;
          let offset = 0;
          while (offset < item.value.length) {
            const input = item.value.subarray(offset, Math.min(offset + 65536, item.value.length));
            const event = this.codec.receiveStep(input, false);
            await this.event(event, input); offset += event.consumed;
            if (event.consumed === 0 && event.written === 0 && event.tag === 1) throw new Error("input decoder made no progress");
          }
        }
      } catch (error) { await reader.cancel().catch(() => {}); throw error; }
      finally { reader.releaseLock(); }
    }
    for (let attempts = 0; !this.eof && attempts < 128; attempts++) await this.event(this.codec.receiveStep(EMPTY, true), EMPTY);
    if (!this.eof || this.current || this.objects !== this.expected) throw new Error("intake did not close exactly");
    this.flushObjects();
    const bytes = this.writer ? await this.writer.finish() : 0;
    if (this.writer && (!this.pack || bytes !== this.lastEnd + this.codec.idBytes)) throw new Error("stored pack extent differs");
    this.writer = null;
    this.store.exec("UPDATE stages SET phase='decoded',pack=?,bytes=?,selected=?,expected=? WHERE id=?", this.pack, bytes, this.codec.receiveCapabilities(), this.expected, this.stage);
    return bytes;
  }

  async validate() {
    // Plain objects already have canonical IDs from intake; scan their links
    // and warm bounded canonical caches before processing ready deltas.
    for (;;) {
      const rows = this.store.exec("SELECT * FROM stage_objects WHERE stage=? AND verified=1 AND scanned=0 ORDER BY node LIMIT 128", this.stage);
      if (!rows.length) break;
      for (const row of rows) await this.source.process(row, true);
    }
    this.store.exec("UPDATE stage_objects AS e SET ready=1 WHERE e.stage=? AND e.verified=0 AND ((e.wire_base_offset IS NOT NULL AND EXISTS(SELECT 1 FROM stage_objects b WHERE b.stage=e.stage AND b.node=e.wire_base_offset AND b.verified=1)) OR (e.wire_base_id IS NOT NULL AND (EXISTS(SELECT 1 FROM stage_objects b WHERE b.stage=e.stage AND b.oid=e.wire_base_id AND b.verified=1) OR EXISTS(SELECT 1 FROM stage_objects b JOIN stages p ON p.id=b.stage WHERE b.oid=e.wire_base_id AND p.phase='published'))))", this.stage);
    for (;;) {
      const rows = this.store.exec("SELECT * FROM stage_objects WHERE stage=? AND verified=0 AND ready=1 ORDER BY node LIMIT 128", this.stage);
      if (!rows.length) break;
      for (const row of rows) await this.delta(row);
    }
    if (this.store.exec("SELECT node FROM stage_objects WHERE stage=? AND (verified=0 OR scanned=0) LIMIT 1", this.stage).length) throw new Error("unresolved or cyclic delta base");
    const missing = this.store.exec("SELECT e.dst,e.kind FROM stage_edges e WHERE e.stage=? AND NOT EXISTS(SELECT 1 FROM stage_objects t WHERE t.stage=e.stage AND t.oid=e.dst AND t.verified=1 AND t.kind=e.kind) AND NOT EXISTS(SELECT 1 FROM stage_objects t JOIN stages p ON p.id=t.stage WHERE t.oid=e.dst AND t.kind=e.kind AND p.phase='published') LIMIT 1", this.stage)[0];
    if (missing) throw new Error("object link target is missing or has the wrong kind");
    this.store.exec("UPDATE stages SET phase='verified' WHERE id=?", this.stage);
  }

  async delta(row) {
    const base = this.source.base(row);
    if (!base?.verified) throw new Error("ready delta has an unverified base");
    const prefix = await this.source.representation(row, 0, Math.min(32, row.packed_size));
    const header = this.codec.deltaHeader(prefix, { size: base.size, depth: base.depth });
    row.kind = base.kind; row.size = integer(header.b); row.depth = integer(header.c);
    row.base_stage = base.stage; row.base_node = base.node; row.base_oid = base.oid;
    this.store.exec("UPDATE stage_objects SET kind=?,size=?,depth=?,base_stage=?,base_node=?,base_oid=? WHERE stage=? AND node=?", row.kind, row.size, row.depth, row.base_stage, row.base_node, row.base_oid, this.stage, row.node);
    await this.source.warm(base);
    const id = await this.source.process(row, true);
    // Indexed dependency updates avoid repeatedly scanning all unresolved
    // objects when a pack uses forward reference-delta chains.
    this.store.exec("UPDATE stage_objects SET ready=1 WHERE stage=? AND wire_base_offset=? AND verified=0", this.stage, row.node);
    this.store.exec("UPDATE stage_objects SET ready=1 WHERE stage=? AND wire_base_id=? AND verified=0", this.stage, id);
  }

  async run(request) {
    try {
      const bytes = await this.decode(request);
      await this.validate();
      return {
        stage: this.stage, pack: this.pack, packBytes: bytes, objects: this.objects,
        commands: this.commands, options: this.options, refsUpdated: 0, quarantine: true,
        queries: this.source.queries, representationReads: this.source.storageReads,
        historicalModes: this.source.historicalModes,
        caches: { canonicalPeak: this.source.canonical.peak, representationPeak: this.source.representations.peak, chunkPeak: this.source.chunks.peak },
        wasmMemoryBytes: this.codec.memory.byteLength,
      };
    } catch (error) {
      console.error(JSON.stringify({ event: "receive-rejected", stage: this.stage, phase: this.inPack ? "pack" : this.eof ? "validation" : "controls", objects: this.objects, expected: this.expected, lastEnd: this.lastEnd, queries: this.source.queries, error: String(error.message ?? error), codecError: error.code ?? null }));
      error.commands = this.store.exec("SELECT name FROM stage_commands WHERE stage=? ORDER BY seq", this.stage);
      error.selected = this.store.exec("SELECT selected FROM stages WHERE id=?", this.stage)[0]?.selected ?? 0;
      await this.store.abandon(this.stage);
      throw error;
    } finally {
      this.codec.receiveCancel(); this.codec.sourceCancel(); this.codec.rangeCancel(); this.codec.graphCancel();
      await this.store.clearTemporary(this.stage);
    }
  }
}
