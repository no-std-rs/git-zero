import { hex, integer } from "./codec.mjs";
import { bytes, PAGE_BYTES } from "./storage.mjs";
import { ObjectSource } from "./source.mjs";

const encode = (value) => new TextEncoder().encode(value);
const fromHex = (value) => Uint8Array.from(value.match(/../g), (pair) => Number.parseInt(pair, 16));

// All Git framing/options arrive as typed Rust events. SQLite holds the work
// queues; the host never loads a repository-sized object selection into memory.
export class FetchJob {
  constructor(store, codec, version) {
    this.store = store; this.codec = codec; this.version = version;
    this.temporary = crypto.randomUUID();
    this.source = new ObjectSource(store, codec, this.temporary, store.namespace);
  }
  reset() { for (const table of ["fetch_wants", "fetch_haves", "fetch_common", "fetch_selected"]) this.store.exec(`DELETE FROM ${table}`); }
  async *references(v2 = false) {
    this.codec.uploadInit(this.version);
    yield* this.refRows(v2);
  }
  async *refRows(v2) {
    let first = true;
    const state = this.store.exec("SELECT head FROM repository WHERE id=1")[0];
    const head = this.store.exec("SELECT oid FROM refs WHERE name=?", state.head)[0];
    if (head || v2) {
      yield this.codec.uploadRef({ id: head ? fromHex(head.oid) : undefined, name: encode("HEAD"), target: bytes(state.head) }, { first, v2 });
      if (head) first = false;
    }
    for (const row of this.store.references()) {
      yield this.codec.uploadRef({ id: fromHex(row.oid), name: row.name, peeled: row.peeled ? fromHex(row.peeled) : undefined }, { first, v2 });
      first = false;
    }
    if (!v2 && first) yield this.codec.uploadRef({ name: new Uint8Array(), target: bytes(state.head) }, { first: true });
    yield this.codec.uploadFlush();
  }
  async decode(request) {
    this.reset(); this.codec.uploadInit(this.version);
    let stream = request.body;
    const coding = request.headers.get("content-encoding");
    if (coding && coding !== "identity") {
      if (coding !== "gzip" || !stream) throw new Error("unsupported HTTP content coding");
      stream = stream.pipeThrough(new DecompressionStream("gzip"));
    }
    const reader = stream?.getReader();
    if (reader) {
      try {
        for (;;) {
          const item = await reader.read(); if (item.done) break;
          for (let offset = 0; offset < item.value.length; offset += PAGE_BYTES) this.codec.uploadAppend(item.value.subarray(offset, offset + PAGE_BYTES));
        }
      } catch (error) { await reader.cancel().catch(() => {}); throw error; }
      finally { reader.releaseLock(); }
    }
    const parsed = this.codec.uploadParse();
    this.command = parsed.flags & 255; this.band = (parsed.flags >> 8) & 255;
    this.done = !!(parsed.flags & (1 << 16));
    this.includeTags = !!(parsed.flags & (1 << 17));
    if (parsed.a !== 0n || (parsed.flags & ((1 << 18) | (1 << 19) | (1 << 20)))) throw new Error("fetch extension is not yet enabled");
    for (;;) {
      const event = this.codec.uploadNext(); if (event.tag === 74) break;
      const id = hex(event.id);
      if (event.tag === 71) {
        if (this.store.exec("SELECT 1 FROM fetch_wants WHERE oid=?", id).length) continue;
        // Both direct and peeled tips are indexed and updated in the same ref
        // transaction. Repeated wants do not traverse the reference catalog.
        const allowed = this.store.exec("SELECT 1 FROM refs WHERE oid=? OR peeled=? LIMIT 1", id, id).length;
        if (!allowed || !this.store.published(id)) throw new Error("want is not advertised");
        this.store.exec("INSERT OR IGNORE INTO fetch_wants(oid) VALUES(?)", id);
      } else if (event.tag === 72) {
        if (this.store.published(id)?.kind === 1) this.store.exec("INSERT OR IGNORE INTO fetch_haves(oid) VALUES(?)", id);
      } else throw new Error("shallow fetch is not yet enabled");
    }
    if (this.done && !this.reusable()) this.select();
  }
  reusable() {
    if (this.store.exec("SELECT 1 FROM fetch_haves LIMIT 1").length) return false;
    const sources = this.store.exec("SELECT COUNT(*) AS count,COALESCE(SUM(expected),0) AS objects,COALESCE(SUM(bytes),0) AS bytes FROM stages WHERE phase='published' AND reusable=1 AND expected>0")[0];
    const repository = this.store.exec("SELECT object_count FROM repository WHERE id=1")[0];
    if (!sources.count || sources.objects !== repository.object_count) return false;
    const length = sources.bytes - (sources.count - 1) * (12 + this.codec.idBytes);
    if (!Number.isSafeInteger(length) || length > this.codec.limits.maxPackBytes || sources.objects > this.codec.limits.maxObjects) throw new Error("full pack exceeds resource limits");
    this.rawSources = sources; return true;
  }
  walk(table, excludeCommon = false, snapshots = false) {
    for (;;) {
      const pending = this.store.exec(`SELECT oid FROM ${table} WHERE walked=0 ORDER BY oid LIMIT 128`);
      if (!pending.length) break;
      for (const item of pending) {
        const row = this.store.published(item.oid);
        if (!row) throw new Error("published closure is incomplete");
        this.store.exec(`UPDATE ${table} SET stage=?,node=?,walked=1 WHERE oid=?`, row.stage, row.node, row.oid);
        this.store.exec(`INSERT OR IGNORE INTO ${table}(oid) SELECT e.dst FROM stage_edges e WHERE e.stage=? AND e.node=?${snapshots ? " AND e.relation<>1" : ""}${excludeCommon ? " AND NOT EXISTS(SELECT 1 FROM fetch_common c WHERE c.oid=e.dst)" : ""}`, row.stage, row.node);
      }
    }
  }
  select() {
    this.store.exec("INSERT OR IGNORE INTO fetch_common(oid) SELECT oid FROM fetch_haves");
    // A have cuts wanted commit traversal. Its snapshot also proves trees and
    // blobs already present, without walking every historical parent merely
    // to send a small increment. Unlisted older common history may be resent;
    // this is a valid full pack and never assumes an absent object is present.
    this.walk("fetch_common", false, true);
    this.store.exec("INSERT OR IGNORE INTO fetch_selected(oid) SELECT w.oid FROM fetch_wants w WHERE NOT EXISTS(SELECT 1 FROM fetch_common c WHERE c.oid=w.oid)");
    this.walk("fetch_selected", true);
    if (this.includeTags) {
      for (const ref of this.store.references()) {
        if (ref.peeled && this.store.exec("SELECT 1 FROM fetch_selected WHERE oid=?", ref.peeled).length) this.store.exec("INSERT OR IGNORE INTO fetch_selected(oid) VALUES(?)", ref.oid);
      }
      this.walk("fetch_selected", true);
    }
  }
  async *pack() {
    if (this.rawSources) { yield* this.reusedPack(); return; }
    const count = this.store.exec("SELECT COUNT(*) AS count FROM fetch_selected")[0].count;
    yield this.codec.packStart(count, this.band);
    let afterStage = "", afterNode = -1;
    for (;;) {
      // Physical order maximizes R2 chunk reuse while REF deltas preserve
      // arbitrary output order. Canonical IDs are unique in this projection.
      const rows = this.store.exec("SELECT o.* FROM fetch_selected f JOIN stage_objects o ON o.stage=f.stage AND o.node=f.node WHERE (f.stage,f.node)>(?,?) ORDER BY f.stage,f.node LIMIT 128", afterStage, afterNode);
      if (!rows.length) break;
      for (const row of rows) {
        afterStage = row.stage; afterNode = row.node;
        const delta = row.wire_kind >= 5;
        const baseSelected = delta && this.store.exec("SELECT 1 FROM fetch_selected WHERE oid=?", row.base_oid).length > 0;
        const fallback = delta && !baseSelected;
        yield this.codec.packEntry({ id: fromHex(row.oid), kind: fallback ? row.kind : delta ? 7 : row.kind, base: baseSelected ? fromHex(row.base_oid) : undefined, size: fallback ? row.size : row.packed_size, compressedSize: row.end_offset - row.compressed_offset, fallback });
        if (fallback) {
          await this.source.warm(this.source.base(row));
          await this.source.process(row, false);
          for (let offset = 0; offset < row.size;) {
            const data = await this.source.readRow(row, 0, offset, Math.min(PAGE_BYTES, row.size - offset));
            if (!data.length) throw new Error("canonical fallback made no progress");
            yield this.codec.packFeed(data); offset += data.length;
          }
        } else {
          for (let offset = row.compressed_offset; offset < row.end_offset;) {
            const data = await this.source.chunkAt(row, offset);
            const count = Math.min(PAGE_BYTES, data.length, row.end_offset - offset);
            yield this.codec.packFeed(data.subarray(0, count)); offset += count;
          }
        }
        yield this.codec.packEntryFinish();
      }
    }
    yield this.codec.packFinish();
  }
  async *sourceRange(stage, start, length, passthrough) {
    const end = start + length;
    for (let offset = start; offset < end;) {
      const chunk = await this.source.chunkAt({ stage }, offset);
      const count = Math.min(PAGE_BYTES, chunk.length, end - offset);
      if (!count) throw new Error("stored pack range made no progress");
      const bytes = chunk.subarray(0, count);
      yield passthrough ? this.codec.packChunk(bytes, this.band) : this.codec.packFeed(bytes);
      offset += count;
    }
  }
  async *reusedPack() {
    // Each eligible segment contains only IDs new at its publication. Their
    // disjoint union's size equals the complete catalog, proving base coverage.
    // The authenticated owner can receive extra same-repository objects.
    const single = this.rawSources.count === 1;
    if (!single) yield this.codec.packStart(this.rawSources.objects, this.band);
    let after = "";
    for (;;) {
      const rows = this.store.exec("SELECT id,expected,bytes FROM stages WHERE id>? AND phase='published' AND reusable=1 AND expected>0 ORDER BY id LIMIT 128", after);
      if (!rows.length) break;
      for (const row of rows) {
        after = row.id;
        if (single) yield* this.sourceRange(row.id, 0, row.bytes, true);
        else {
          const range = this.codec.packSegmentStart(row.expected, row.bytes);
          yield* this.sourceRange(row.id, integer(range.a), integer(range.b), false);
          yield this.codec.packSegmentFinish();
        }
      }
    }
    if (!single) yield this.codec.packFinish();
    else if (this.band) yield this.codec.uploadFlush();
  }
  async *response() {
    try {
      if (this.command === 0) { yield this.codec.uploadFlush(); return; }
      if (this.command === 1) { yield* this.refRows(true); return; }
      yield this.codec.uploadAckStart();
      let after = "";
      for (;;) {
        const rows = this.store.exec("SELECT oid FROM fetch_haves WHERE oid>? ORDER BY oid LIMIT 128", after);
        if (!rows.length) break;
        for (const row of rows) { after = row.oid; yield this.codec.uploadAckCommon(fromHex(row.oid)); }
      }
      yield this.codec.uploadAckFinish();
      if (this.done) yield* this.pack();
    } finally {
      this.codec.uploadCancel(); this.codec.packCancel(); this.reset();
      await this.store.clearTemporary(this.temporary);
    }
  }
}
