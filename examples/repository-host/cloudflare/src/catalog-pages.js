// Bounded durable metadata only. Payloads remain in R2; Rust drives contracts.
export const PAGE_PREFIX = "pages/";
export const PAGE_END = "pages0";
export const PAGE_CAPACITY = 32;
const MAX_BLOB_BYTES = 16 * 1024 * 1024;
const MAGIC = [0x47, 0x5a, 0x43, 0x50]; // GZCP, u8 version, u8 count
const DIGEST = /^[0-9a-f]{64}$/;
const PAGE = /^pages\/[0-9a-f]{0,64}$/;

export function count(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("invalid catalog integer");
  return value;
}

export function commonPrefix(left, right) {
  let length = 0;
  while (length < left.length && length < right.length && left[length] === right[length]) length++;
  return length;
}

export function ancestors(digest) {
  if (!DIGEST.test(digest)) throw new Error("invalid catalog digest");
  return Array.from({ length: 65 }, (_, length) => PAGE_PREFIX + digest.slice(0, length));
}

export function decodePage(row) {
  if (!PAGE.test(row.key)) throw new Error("invalid catalog page key");
  const value = row.entries;
  const data = value instanceof ArrayBuffer ? new Uint8Array(value)
    : ArrayBuffer.isView(value) ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength) : null;
  const version = data?.[4], header = version === 2 ? 14 : 6, width = version === 2 ? 36 : 40;
  if (!data || data.length < header || MAGIC.some((byte, i) => data[i] !== byte)
      || ![1, 2].includes(version) || data[5] < 1 || data[5] > PAGE_CAPACITY
      || data.length !== header + width * data[5]) throw new Error("invalid catalog page encoding");
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const prefix = row.key.slice(PAGE_PREFIX.length);
  const entries = [];
  const generation = version === 2 ? view.getBigUint64(6, true) : null;
  if (generation !== null && generation > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("invalid catalog acknowledgment generation");
  entries.appliedGeneration = generation === null ? null : Number(generation);
  let total = 0, previous = "";
  for (let offset = header; offset < data.length; offset += width) {
    let digest = "";
    for (const byte of data.subarray(offset, offset + 32)) digest += byte.toString(16).padStart(2, "0");
    const encodedSize = version === 2 ? BigInt(view.getUint32(offset + 32, true)) : view.getBigUint64(offset + 32, true);
    if (encodedSize > BigInt(version === 2 ? MAX_BLOB_BYTES : Number.MAX_SAFE_INTEGER) || digest <= previous || !digest.startsWith(prefix)) {
      throw new Error("invalid catalog page member");
    }
    const size = Number(encodedSize);
    total = count(total + size);
    entries.push({ digest, size });
    previous = digest;
  }
  if (count(row.size) !== total) throw new Error("catalog page size mismatch");
  count(row.peak_bytes);
  return entries;
}

export function encodePage(key, entries, peak, appliedGeneration = null) {
  if (!PAGE.test(key) || !entries.length || entries.length > PAGE_CAPACITY) throw new Error("invalid catalog page bound");
  const version = appliedGeneration === null ? 1 : 2, header = version === 2 ? 14 : 6, width = version === 2 ? 36 : 40;
  if (appliedGeneration !== null) count(appliedGeneration);
  const data = new Uint8Array(header + width * entries.length);
  data.set(MAGIC);
  data[4] = version;
  data[5] = entries.length;
  const view = new DataView(data.buffer);
  if (version === 2) view.setBigUint64(6, BigInt(appliedGeneration), true);
  let size = 0, previous = "";
  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    if (!DIGEST.test(entry.digest) || entry.digest <= previous
        || !entry.digest.startsWith(key.slice(PAGE_PREFIX.length))) throw new Error("invalid catalog page order");
    count(entry.size);
    if (version === 2 && entry.size > MAX_BLOB_BYTES) throw new Error("invalid catalog v2 member size");
    size = count(size + entry.size);
    const offset = header + i * width;
    for (let j = 0; j < 32; j++) data[offset + j] = Number.parseInt(entry.digest.slice(j * 2, j * 2 + 2), 16);
    if (version === 2) view.setUint32(offset + 32, entry.size, true);
    else view.setBigUint64(offset + 32, BigInt(entry.size), true);
    previous = entry.digest;
  }
  return { key, size, peak_bytes: count(peak), entries: data.buffer };
}

export function splitPage(entries) {
  if (entries.length !== PAGE_CAPACITY + 1) throw new Error("invalid catalog split bound");
  const length = commonPrefix(entries[0].digest, entries.at(-1).digest) + 1;
  if (length > 64) throw new Error("duplicate catalog split member");
  const groups = [];
  for (const entry of entries) {
    const key = PAGE_PREFIX + entry.digest.slice(0, length);
    if (groups.at(-1)?.key !== key) groups.push({ key, entries: [] });
    groups.at(-1).entries.push(entry);
  }
  if (groups.length < 2 || groups.length > 16) throw new Error("invalid catalog split fanout");
  return groups;
}
