const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
export const MAX_SNAPSHOT_BYTES = 8192;
const MAX_RECORD_BYTES = MAX_SNAPSHOT_BYTES + 128;
const PREFIX = "snapshots/v1/";

export class SnapshotError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export function decodeSnapshot(bytes) {
  if (bytes.length > MAX_SNAPSHOT_BYTES) throw new Error("Snapshot exceeds 8 KiB");
  const parts = JSON.parse(decoder.decode(bytes));
  if (!Array.isArray(parts) || parts.length !== 4 || parts.some((part) => typeof part !== "string")) {
    throw new Error("Invalid snapshot data");
  }
  return parts;
}

function binding(env, backend) {
  const value = backend === "kv" ? env.SNAPSHOTS_KV : backend === "r2" ? env.SNAPSHOTS_R2 : undefined;
  if (!value || typeof value.get !== "function" || typeof value.put !== "function") {
    throw new SnapshotError("Snapshot storage is not configured", 503);
  }
  return value;
}

async function digest(bytes) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export async function persistSnapshot(bytes, env) {
  const backend = env.SNAPSHOT_STORAGE;
  const store = binding(env, backend);
  const ttl = Number(env.SNAPSHOT_TTL_SECONDS ?? 86400);
  if (!Number.isSafeInteger(ttl) || ttl < 60 || ttl > 31536000) {
    throw new SnapshotError("Snapshot retention must be 60 through 31536000 seconds", 503);
  }
  // The expiry is part of the immutable, authenticated record. Recreating the
  // same files later cannot revive an expired URL or replace its contents.
  const expiresAt = Math.floor(Date.now() / 1000) + ttl;
  const record = encoder.encode(JSON.stringify({ version: 1, expiresAt, parts: decodeSnapshot(bytes) }));
  const id = await digest(record);
  try {
    if (backend === "kv") await store.put(`${PREFIX}${id}`, record.buffer, { expirationTtl: ttl });
    else await store.put(`${PREFIX}${id}`, record, { sha256: id, httpMetadata: { contentType: "application/json" } });
  } catch {
    throw new SnapshotError("Snapshot storage is unavailable", 503);
  }
  return { path: `/s/${backend}/${id}/repo.git`, expiresAt };
}

async function readBounded(stream) {
  if (!stream || typeof stream.getReader !== "function") throw new Error("Invalid stored snapshot");
  const reader = stream.getReader();
  const bytes = new Uint8Array(MAX_RECORD_BYTES);
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return bytes.subarray(0, length);
      if (length + value.length > bytes.length) throw new Error("Stored snapshot exceeds limit");
      bytes.set(value, length);
      length += value.length;
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  } finally {
    reader.releaseLock();
  }
}

export async function loadSnapshot(env, backend, id) {
  const store = binding(env, backend);
  let stored;
  try {
    stored = await store.get(`${PREFIX}${id}`, ...(backend === "kv" ? ["stream"] : []));
  } catch {
    throw new SnapshotError("Snapshot storage is unavailable", 503);
  }
  if (stored === null) throw new SnapshotError("Snapshot not found, expired or not yet available", 404);
  let record;
  try {
    const bytes = await readBounded(backend === "kv" ? stored : stored.body);
    if (await digest(bytes) !== id) throw new Error("Stored snapshot digest mismatch");
    record = JSON.parse(decoder.decode(bytes));
    if (record.version !== 1 || !Number.isSafeInteger(record.expiresAt)) throw new Error("Invalid stored snapshot");
    // Reuse the inline format's schema and size limit after unwrapping storage.
    record.parts = decodeSnapshot(encoder.encode(JSON.stringify(record.parts)));
  } catch {
    throw new SnapshotError("Stored snapshot failed integrity validation", 502);
  }
  if (record.expiresAt <= Math.floor(Date.now() / 1000)) throw new SnapshotError("Snapshot expired", 410);
  return record.parts;
}
