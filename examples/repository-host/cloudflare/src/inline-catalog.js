// One bounded durable root owner. Payloads stay in R2. NULL selects the
// normalized catalog; a non-NULL zero-member owner still preserves its peak.
import { count, PAGE_CAPACITY } from "./catalog-pages.js";
export const INLINE_HEADER = "x-git-zero-internal-catalog-inline";
export const INLINE_CAPABILITY = { version: 1, members: PAGE_CAPACITY, manifest_bytes: 8192 };
const MAGIC = [0x47, 0x5a, 0x49, 0x52, 1]; // GZIR v1
const MAX_BLOB_BYTES = 16 * 1024 * 1024;

export function encodeInline(entries, peak, appliedGeneration) {
  if (entries.length > PAGE_CAPACITY) throw new Error("inline root capacity exceeded");
  const data = new Uint8Array(22 + 36 * entries.length), view = new DataView(data.buffer);
  data.set(MAGIC); data[5] = entries.length;
  view.setBigUint64(6, BigInt(count(peak)), true);
  view.setBigUint64(14, BigInt(count(appliedGeneration)), true);
  let previous = "", size = 0;
  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    if (!/^[0-9a-f]{64}$/.test(entry.digest) || entry.digest <= previous
        || count(entry.size) > MAX_BLOB_BYTES) throw new Error("invalid inline member");
    const offset = 22 + i * 36;
    for (let j = 0; j < 32; j++) data[offset + j] = Number.parseInt(entry.digest.slice(j * 2, j * 2 + 2), 16);
    view.setUint32(offset + 32, entry.size, true);
    size = count(size + entry.size); previous = entry.digest;
  }
  return data.buffer;
}

export function decodeInline(value, generation) {
  const data = value instanceof ArrayBuffer ? new Uint8Array(value)
    : ArrayBuffer.isView(value) ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength) : null;
  if (!data || data.length < 22 || MAGIC.some((byte, i) => data[i] !== byte)
      || data[5] > PAGE_CAPACITY || data.length !== 22 + 36 * data[5]) throw new Error("invalid inline root encoding");
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const safe = (offset) => {
    const number = view.getBigUint64(offset, true);
    if (number > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("invalid inline integer");
    return Number(number);
  };
  const peak = safe(6), appliedGeneration = safe(14), entries = [];
  if (appliedGeneration > count(generation)) throw new Error("inline acknowledgment exceeds generation");
  let previous = "", size = 0;
  for (let offset = 22; offset < data.length; offset += 36) {
    let digest = "";
    for (const byte of data.subarray(offset, offset + 32)) digest += byte.toString(16).padStart(2, "0");
    const memberSize = view.getUint32(offset + 32, true);
    if (digest <= previous || memberSize > MAX_BLOB_BYTES) throw new Error("invalid inline root member");
    size = count(size + memberSize); entries.push({ digest, size: memberSize }); previous = digest;
  }
  if (peak < size) throw new Error("inline peak below catalog bytes");
  entries.appliedGeneration = appliedGeneration;
  return { entries, size, peak, appliedGeneration };
}

// Preserve an independently valid high-water witness even when the rest of a
// damaged envelope cannot certify membership or replay acknowledgment.
export function inlinePeak(value) {
  const data = value instanceof ArrayBuffer ? new Uint8Array(value)
    : ArrayBuffer.isView(value) ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength) : null;
  if (!data || data.length < 14 || MAGIC.some((byte, i) => data[i] !== byte)) return 0;
  const number = new DataView(data.buffer, data.byteOffset, data.byteLength).getBigUint64(6, true);
  return number <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(number) : 0;
}
