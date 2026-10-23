import { readBytes } from "./bytes.mjs";

export const MAX_PACK_BYTES = 147456;
const HEADER = 68;
const DIGEST = 32;
const MAGIC = new Uint8Array([71, 90, 65, 67, 0, 0, 0, 1]);

export class PackCacheError extends Error {
  constructor(message, status = 503) { super(message); this.status = status; }
}

function integer(value, minimum, maximum, label) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new TypeError(`Invalid pack cache ${label}`);
  }
  return value;
}

function equal(first, second) {
  return first.length === second.length && first.every((value, index) => value === second[index]);
}

function keyBytes(key) {
  if (typeof key !== "string" || !/^[0-9a-f]{96}$/.test(key)) throw new PackCacheError("Invalid selected-pack key", 502);
  const bytes = Uint8Array.from(key.match(/../g), (pair) => Number.parseInt(pair, 16));
  if (!equal(bytes.subarray(0, 5), new Uint8Array([71, 90, 80, 67, 1])) ||
      ![1, 2].includes(bytes[5]) || !equal(bytes.subarray(6, 12), new Uint8Array([0, 0, 0, 0, 0, 1]))) {
    throw new PackCacheError("Unsupported selected-pack key", 502);
  }
  return bytes;
}

async function hash(bytes) { return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)); }

export async function encodePackRecord(key, pack, expiresAt) {
  const identity = keyBytes(key);
  if (!(pack instanceof Uint8Array) || pack.length > MAX_PACK_BYTES) throw new PackCacheError("Pack exceeds cache limit", 413);
  integer(expiresAt, 1, Number.MAX_SAFE_INTEGER, "expiry");
  const record = new Uint8Array(HEADER + pack.length + DIGEST);
  record.set(MAGIC);
  const view = new DataView(record.buffer);
  view.setBigUint64(8, BigInt(expiresAt));
  record.set(identity, 16);
  view.setUint32(64, pack.length);
  record.set(pack, HEADER);
  record.set(await hash(record.subarray(0, -DIGEST)), record.length - DIGEST);
  return record;
}

export async function decodePackRecord(key, record, maximum, now) {
  const identity = keyBytes(key);
  if (!(record instanceof Uint8Array) || record.length < HEADER + DIGEST || record.length > HEADER + maximum + DIGEST) {
    throw new PackCacheError("Invalid pack cache record size", 502);
  }
  const view = new DataView(record.buffer, record.byteOffset, record.byteLength);
  const expiry = view.getBigUint64(8);
  if (!equal(record.subarray(0, 8), MAGIC) || !equal(record.subarray(16, 64), identity) ||
      view.getUint32(64) !== record.length - HEADER - DIGEST || expiry > BigInt(Number.MAX_SAFE_INTEGER) ||
      !equal(await hash(record.subarray(0, -DIGEST)), record.subarray(-DIGEST))) {
    throw new PackCacheError("Pack cache record failed integrity validation", 502);
  }
  if (Number(expiry) <= now) return null;
  // Independent bytes, never a view into mutable backend or another response.
  return record.slice(HEADER, -DIGEST);
}

function aborted(signal) {
  if (signal?.aborted) throw new PackCacheError("Git request cancelled", 499);
}

// Observe cancellation even when the storage API has no AbortSignal parameter.
// A put already in progress may finish publishing its complete immutable record.
function awaiting(operation, signal) {
  if (!signal) return operation;
  aborted(signal);
  return new Promise((resolve, reject) => {
    const abort = () => reject(new PackCacheError("Git request cancelled", 499));
    signal.addEventListener("abort", abort, { once: true });
    Promise.resolve(operation).then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

/// Create one trusted cache policy per isolate/namespace, then reuse it across
/// requests. Scalar configuration is captured now, before any await. Bindings
/// provide atomic complete-value puts. Local flights are admission-bounded;
/// KV eventual consistency may cause another producer after a completed flight.
export function createPackCache({ backend, binding, namespace, maxBytes = MAX_PACK_BYTES,
  ttlSeconds = 3600, maxInflight = 16, maxWaiters = 64, now = () => Math.floor(Date.now() / 1000) }) {
  if (!["kv", "r2"].includes(backend) || !binding || typeof binding.get !== "function" || typeof binding.put !== "function") {
    throw new TypeError("A KV or R2 pack cache binding is required");
  }
  if (typeof namespace !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(namespace)) throw new TypeError("An explicit private pack cache namespace is required");
  integer(maxBytes, 32, MAX_PACK_BYTES, "byte limit");
  integer(ttlSeconds, 60, 31536000, "retention");
  integer(maxInflight, 1, 1024, "producer limit");
  integer(maxWaiters, 0, 4096, "waiter limit");
  if (typeof now !== "function") throw new TypeError("Invalid pack cache clock");
  const counters = { hits: 0, misses: 0, waits: 0, writes: 0, failures: 0, producers: 0, peakProducers: 0 };
  const storage = new PackStorage({ backend, binding, namespace, maxBytes, ttlSeconds, now }, counters);
  const coordinator = new PackCoordinator(storage, maxInflight, maxWaiters, counters);
  return Object.freeze({
    maxBytes,
    statistics: () => coordinator.statistics(),
    resolve: (key, generate, authenticate, signal) => coordinator.resolve(key, generate, authenticate, signal),
  });
}

class PackStorage {
  constructor({ backend, binding, namespace, maxBytes, ttlSeconds, now }, counters) {
    Object.assign(this, { backend, namespace, maxBytes, ttlSeconds, now, counters });
    this.get = binding.get.bind(binding);
    this.put = binding.put.bind(binding);
  }
  path(key) { return `selected-packs/v1/${this.namespace}/${key}`; }
  clock() { return integer(this.now(), 0, Number.MAX_SAFE_INTEGER - this.ttlSeconds, "clock"); }

  async load(key, signal) {
    aborted(signal);
    let stored;
    try {
      const operation = Promise.resolve(this.get(this.path(key), ...(this.backend === "kv" ? ["stream"] : []))).then((value) => {
        if (signal?.aborted && value !== null) {
          const body = this.backend === "kv" ? value : value.body;
          body.cancel().catch(() => {});
        }
        return value;
      });
      stored = await awaiting(operation, signal);
    }
    catch (error) {
      if (error instanceof PackCacheError) throw error;
      throw new PackCacheError("Pack cache read failed");
    }
    aborted(signal);
    if (stored === null) return null;
    const stream = this.backend === "kv" ? stored : stored.body;
    try {
      if (this.backend === "r2" && stored.size > HEADER + this.maxBytes + DIGEST) {
        await stream.cancel();
        throw new Error("Oversized R2 pack record");
      }
      const record = await awaiting(readBytes(stream, HEADER + this.maxBytes + DIGEST, signal), signal);
      aborted(signal);
      return await decodePackRecord(key, record, this.maxBytes, this.clock());
    } catch (error) {
      aborted(signal);
      if (error instanceof PackCacheError) throw error;
      throw new PackCacheError("Pack cache read or record validation failed", 502);
    }
  }

  async store(key, pack, signal) {
    const record = await encodePackRecord(key, pack, this.clock() + this.ttlSeconds);
    aborted(signal);
    try {
      await awaiting(this.backend === "kv"
        ? this.put(this.path(key), record, { expirationTtl: this.ttlSeconds })
        : this.put(this.path(key), record, { httpMetadata: { contentType: "application/octet-stream" } }), signal);
      this.counters.writes++;
    } catch (error) {
      if (error instanceof PackCacheError) throw error;
      throw new PackCacheError("Pack cache publication failed");
    }
    aborted(signal);
  }

}

class PackCoordinator {
  constructor(storage, maxInflight, maxWaiters, counters) {
    Object.assign(this, { storage, maxInflight, maxWaiters, counters });
    this.flights = new Map();
    this.waiters = 0;
  }
  statistics() {
    return Object.freeze({ ...this.counters, inflight: this.flights.size, waiters: this.waiters });
  }

  async resolve(key, generate, authenticate, signal) {
    keyBytes(key);
    try {
      aborted(signal);
      while (this.flights.has(key)) {
        if (this.waiters >= this.maxWaiters) throw new PackCacheError("Pack cache waiter limit exhausted");
        this.counters.waits++;
        this.waiters++;
        try { await awaiting(this.flights.get(key), signal); }
        finally { this.waiters--; }
        aborted(signal);
      }
      // Register before the first await so every same-key miss observes it.
      if (this.flights.size >= this.maxInflight) throw new PackCacheError("Pack cache producer limit exhausted");
      let release;
      const flight = new Promise((resolve) => { release = resolve; });
      this.flights.set(key, flight);
      this.counters.producers++;
      this.counters.peakProducers = Math.max(this.counters.peakProducers, this.counters.producers);
      try {
        let pack = await this.storage.load(key, signal);
        aborted(signal);
        if (pack !== null) {
          await authenticate(pack);
          aborted(signal);
          this.counters.hits++;
          return "hit";
        }
        this.counters.misses++;
        pack = await generate();
        aborted(signal);
        if (!(pack instanceof Uint8Array) || pack.length > this.storage.maxBytes) throw new PackCacheError("Generated pack exceeds cache limit", 413);
        await authenticate(pack);
        aborted(signal);
        await this.storage.store(key, pack, signal);
        return "miss";
      } finally {
        this.counters.producers--;
        this.flights.delete(key);
        release();
      }
    } catch (error) {
      this.counters.failures++;
      throw error;
    }
  }
}

export function packCachePolicy(module, cache) {
  if (cache === undefined || cache === null) return null;
  if (typeof cache.resolve !== "function" || typeof cache.statistics !== "function") throw new TypeError("Invalid host pack cache");
  integer(cache.maxBytes, 32, MAX_PACK_BYTES, "byte limit");
  const exports = WebAssembly.Module.exports(module);
  for (const name of ["cache_prepare", "cache_key", "cache_read", "cache_attach", "cache_ptr", "cache_capacity"]) {
    if (!exports.some((entry) => entry.name === name && entry.kind === "function")) throw new TypeError(`Pack cache requires Wasm export ${name}`);
  }
  return cache;
}

function packFailure(message, status) {
  return new Response(`${message}\n`, { status, headers: { "Cache-Control": "no-store" } });
}

// The prepared Wasm instance and every backing buffer belong to this one call.
// No INPUT view or mutable host repository is read again after the first await.
export async function cachedResponse(exports, length, method, policy, revisions, cache, signal, respond) {
  const prepared = exports.cache_prepare(length, method === "POST" ? 1 : 0, policy ? 1 : 0,
    policy?.maxWantLines ?? 64, revisions ? 1 : 0, revisions?.maxExclusions ?? 0, revisions?.graphReadLimit ?? 0);
  if (prepared < 0) return respond(exports, prepared);
  if (prepared === 0) return respond(exports, 3);
  try {
    if (exports.cache_key() !== 48) throw new PackCacheError("Selected-pack preparation failed", 502);
    const key = [...new Uint8Array(exports.memory.buffer, exports.output_ptr(), 48)]
      .map((byte) => byte.toString(16).padStart(2, "0")).join("");
    await cache.resolve(key, () => {
      const pack = new Uint8Array(cache.maxBytes);
      let size = 0;
      while (true) {
        aborted(signal);
        const count = exports.cache_read(16384);
        if (count < 0) throw new PackCacheError("Selected-pack encoding failed", 502);
        if (count === 0) return pack.subarray(0, size);
        if (size + count > pack.length) throw new PackCacheError("Selected pack exceeds cache limit", 413);
        pack.set(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count), size);
        size += count;
      }
    }, (pack) => {
      const pointer = exports.cache_ptr();
      if (!pointer || pack.length > exports.cache_capacity()) throw new PackCacheError("Invalid prepared pack capacity", 502);
      new Uint8Array(exports.memory.buffer, pointer, pack.length).set(pack);
      const result = exports.cache_attach(pack.length, cache.maxBytes);
      if (result < 0) throw new PackCacheError("Selected pack failed exact-set verification", result === -1 ? 413 : 502);
    }, signal);
    aborted(signal);
    return respond(exports, 3);
  } catch (error) {
    exports.stream_cancel();
    return packFailure(error instanceof PackCacheError ? error.message : "Pack cache failed",
      error instanceof PackCacheError ? error.status : 503);
  }
}
