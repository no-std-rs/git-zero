// Runtime serialization only: Git parsing, hashing and reconstruction execute
// in the no_std Rust module. Every HTTP operation owns its own Codec instance.
const INPUT = 65536;
const FRAME = 128;
const U64_MAX = (1n << 64n) - 1n;
const DEFAULTS = Object.freeze({
  maxControlBytes: 4 * 1024 * 1024, maxPackBytes: 64 * 1024 ** 3,
  maxObjects: 100_000_000, maxObjectBytes: 8 * 1024 ** 3,
  maxRatio: 65536, ratioSlack: 32768, maxCommands: 16384,
  maxShallow: 16384, maxOptions: 16384, maxDepth: 128,
  maxWorkBytes: 1024 ** 4, maxInstructions: 100_000_000,
  maxReadBytes: INPUT, maxLinks: 100_000_000, nameScratch: 131072,
  capabilities: 0,
});

export class CodecError extends Error {
  constructor(code, operation) { super(`Rust codec ${operation} failed (${code})`); this.code = code; }
}

function wide(view, offset, value) {
  if (typeof value === "number" && (!Number.isSafeInteger(value) || value < 0)) throw new RangeError("unsafe integer in ABI frame");
  value = BigInt(value);
  if (value < 0n || value > U64_MAX) throw new RangeError("integer exceeds ABI field");
  view.setBigUint64(offset, value, true);
}

function fields(length) {
  const bytes = new Uint8Array(length);
  const view = new DataView(bytes.buffer);
  return { bytes, word: (offset, value) => {
    if (!Number.isInteger(value) || value < 0 || value > 0xffffffff) throw new RangeError("invalid ABI word");
    view.setUint32(offset, value, true);
  }, wide: (offset, value) => wide(view, offset, value) };
}

export function integer(value) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new RangeError("storage offset exceeds safe integer range");
  return number;
}

export function hex(bytes) { return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(""); }

export class Codec {
  constructor(module, format = 0, limits = {}) {
    if (![0, 1].includes(format)) throw new RangeError("unsupported object format");
    this.exports = new WebAssembly.Instance(module, {}).exports;
    this.memory = this.exports.memory.buffer;
    this.format = format;
    this.idBytes = format === 0 ? 20 : 32;
    this.limits = { ...DEFAULTS, ...limits };
    if (this.exports.host_input_capacity() !== INPUT || this.exports.host_output_capacity() !== 131072) throw new Error("unexpected codec ABI capacities");
  }

  stage(bytes) {
    if (!(bytes instanceof Uint8Array) || bytes.length > INPUT) throw new RangeError("ABI input exceeds capacity");
    new Uint8Array(this.memory, this.exports.host_input_ptr(), bytes.length).set(bytes);
  }

  invoke(name, ...args) {
    const result = this.exports[name](...args);
    if (this.exports.memory.buffer !== this.memory) throw new Error("codec grew linear memory");
    if (result < 0) throw new CodecError(result, name);
    return result;
  }

  event(name, bytes, ...args) {
    if (bytes !== null) this.stage(bytes);
    const length = this.invoke(name, ...(bytes === null ? [] : [bytes.length]), ...args);
    if (length < FRAME || length > 131072) throw new Error("invalid event length");
    const raw = new Uint8Array(this.memory, this.exports.host_output_ptr(), length).slice();
    const view = new DataView(raw.buffer);
    const written = view.getUint32(8, true);
    if (written + FRAME !== length) throw new Error("invalid event payload length");
    const id = raw.slice(48, 48 + this.idBytes);
    return {
      tag: view.getUint32(0, true), consumed: view.getUint32(4, true), written,
      flags: view.getUint32(12, true), a: view.getBigUint64(16, true),
      b: view.getBigUint64(24, true), c: view.getBigUint64(32, true), d: view.getBigUint64(40, true),
      id, other: raw.slice(80, 80 + this.idBytes),
      key: view.getUint32(112, true) === 1 ? { staged: view.getBigUint64(40, true) } : { id },
      data: raw.subarray(FRAME),
    };
  }

  id(frame, offset, id) {
    if (!(id instanceof Uint8Array) || id.length !== this.idBytes) throw new RangeError("mixed object formats in ABI frame");
    frame.bytes.set(id, offset);
  }

  key(frame, tag, staged, id, key) {
    if (key.id !== undefined) { frame.word(tag, 0); this.id(frame, id, key.id); }
    else if (key.staged !== undefined) { frame.word(tag, 1); frame.wide(staged, key.staged); }
    else throw new TypeError("missing object key");
  }

  receiveInit() {
    const f = fields(72), l = this.limits;
    f.word(0, this.format); f.wide(8, l.maxControlBytes); f.wide(16, l.maxPackBytes); f.word(24, l.maxObjects);
    f.wide(32, l.maxObjectBytes); f.wide(40, l.maxRatio); f.wide(48, l.ratioSlack);
    f.word(56, l.maxCommands); f.word(60, l.maxShallow); f.word(64, l.maxOptions); f.word(68, l.capabilities);
    this.stage(f.bytes); this.invoke("host_receive_init", f.bytes.length);
  }
  receiveStep(bytes, eof = false) { return this.event("host_receive_step", bytes, Number(eof)); }
  receiveCapabilities() { return this.invoke("host_receive_capabilities"); }
  receiveCancel() { this.invoke("host_receive_cancel"); }

  advertStart() { return this.event("host_advert_start", null, this.format, this.limits.capabilities).data; }
  advertRef(id, name) {
    const f = fields(32 + name.length); this.id(f, 0, id); f.bytes.set(name, 32);
    return this.event("host_advert_ref", f.bytes).data;
  }
  advertFinish() { return this.event("host_advert_finish", null).data; }
  statusStart(selected, reason = new Uint8Array()) { return this.event("host_status_start", reason, selected).data; }
  statusRef(name, reason = new Uint8Array()) {
    const f = fields(4 + name.length + reason.length); f.word(0, name.length);
    f.bytes.set(name, 4); f.bytes.set(reason, 4 + name.length);
    return this.event("host_status_ref", f.bytes).data;
  }
  statusFinish() { return this.event("host_status_finish", null).data; }
  progress(selected, message) { return this.event("host_progress", message, selected).data; }

  uploadInit(version) { this.invoke("host_upload_init", this.format, version); }
  uploadVersion(header) { const bytes = new TextEncoder().encode(header ?? ""); this.stage(bytes); return this.invoke("host_upload_version", bytes.length, Number(header !== null)); }
  uploadAppend(bytes) { this.stage(bytes); this.invoke("host_upload_append", bytes.length); }
  uploadParse() { return this.event("host_upload_parse", null); }
  uploadNext() { return this.event("host_upload_next", null); }
  uploadAdvert(version, selection = false) { return this.event("host_upload_advert", null, this.format, version, Number(selection)).data; }
  uploadRef(ref, { v2 = false, first = false, selection = false } = {}) {
    const name = ref.name, target = ref.target ?? new Uint8Array();
    const f = fields(80 + name.length + target.length);
    f.word(64, (ref.id ? 1 : 0) | (ref.peeled ? 2 : 0));
    if (ref.id) this.id(f, 0, ref.id);
    if (ref.peeled) this.id(f, 32, ref.peeled);
    f.word(68, name.length); f.word(72, target.length); f.bytes.set(name, 80); f.bytes.set(target, 80 + name.length);
    // IDs are staged at padded ABI slots 0/32, distinct from frame event slots.
    return this.event("host_upload_ref", f.bytes, Number(v2), Number(first), Number(selection)).data;
  }
  uploadFlush() { return this.event("host_upload_flush", null).data; }
  uploadAckStart() { return this.event("host_upload_ack_start", null).data; }
  uploadAckCommon(id) { return this.event("host_upload_ack_common", id).data; }
  uploadAckFinish() { return this.event("host_upload_ack_finish", null).data; }
  uploadCancel() { this.invoke("host_upload_cancel"); }

  packStart(count, band) { return this.event("host_pack_start", null, this.format, count, BigInt(this.limits.maxPackBytes), band).data; }
  packEntry(entry) {
    const f = fields(88); f.word(0, entry.kind); f.word(4, Number(entry.fallback ?? false));
    this.id(f, 8, entry.id); if (entry.base) this.id(f, 40, entry.base);
    f.wide(72, entry.size); f.wide(80, entry.compressedSize ?? 0);
    return this.event("host_pack_entry", f.bytes).data;
  }
  packFeed(bytes) { return this.event("host_pack_feed", bytes).data; }
  packEntryFinish() { return this.event("host_pack_entry_finish", null).data; }
  packFinish() { return this.event("host_pack_finish", null).data; }
  packCancel() { this.invoke("host_pack_cancel"); }

  rangeInit(key, range = null) {
    const f = fields(104), l = this.limits;
    f.word(0, this.format); this.key(f, 4, 8, 16, key);
    f.word(48, range === null ? 0 : 1); f.wide(56, range?.offset ?? 0); f.wide(64, range?.length ?? 0);
    f.wide(72, l.maxObjectBytes); f.wide(80, l.maxWorkBytes); f.wide(88, l.maxInstructions);
    f.word(96, l.maxDepth); f.word(100, l.maxReadBytes);
    this.stage(f.bytes); this.invoke("host_range_init", f.bytes.length);
  }
  rangeDescribe(object) {
    const f = fields(96);
    f.word(0, object.info.kind); f.wide(8, object.info.size);
    f.word(16, object.base === undefined ? 0 : 1);
    if (object.base !== undefined) {
      this.key(f, 20, 24, 32, object.base.key); f.word(64, object.base.info.kind);
      f.wide(72, object.base.info.size); f.word(80, object.base.depth); f.wide(88, object.instructionsSize);
    }
    this.stage(f.bytes); this.invoke("host_range_describe", f.bytes.length);
  }
  rangeRequest() { return this.event("host_range_request", null); }
  rangeFeed(bytes, capacity = INPUT) { return this.event("host_range_feed", bytes, capacity); }
  rangeCancel() { this.invoke("host_range_cancel"); }

  graphInit(info) {
    const f = fields(40), l = this.limits;
    f.word(0, this.format); f.word(4, info.kind); f.wide(8, info.size);
    f.wide(16, l.maxObjectBytes); f.wide(24, l.maxLinks); f.word(32, l.nameScratch);
    this.stage(f.bytes); this.invoke("host_graph_init", f.bytes.length);
  }
  graphFeed(bytes, eof = false) { return this.event("host_graph_feed", bytes, Number(eof)); }
  graphCancel() { this.invoke("host_graph_cancel"); }

  sourceInit(entry, range) {
    const f = fields(64), l = this.limits;
    f.word(0, this.format); f.wide(8, entry.offset); f.wide(16, entry.maxBytes);
    f.wide(24, l.maxObjectBytes); f.wide(32, l.maxRatio); f.wide(40, l.ratioSlack);
    f.wide(48, range.offset); f.wide(56, range.length);
    this.stage(f.bytes); this.invoke("host_source_init", f.bytes.length);
  }
  sourceStep(bytes, eof = false, capacity = INPUT) { return this.event("host_source_step", bytes, Number(eof), capacity); }
  sourceCancel() { this.invoke("host_source_cancel"); }

  deltaHeader(bytes, base) {
    return this.event("host_delta_header", bytes, BigInt(base.size), base.depth, BigInt(this.limits.maxObjectBytes), this.limits.maxDepth);
  }
}
