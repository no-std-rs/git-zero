// Payload bounds per active job. The codec's 6 MiB arena is independent of these.
export const MEMORY_LIMITS = Object.freeze({
  canonicalCacheBytes: 24 * 1024 * 1024, representationCacheBytes: 8 * 1024 * 1024,
  chunkCacheBytes: 16 * 1024 * 1024, cacheEntries: 8192,
  canonicalCaptureBytes: 8 * 1024 * 1024, representationCaptureBytes: 1024 * 1024,
  chunkBytes: 1024 * 1024, refPageRows: 128, metadataEntries: 256,
});

// Smaller defaults for a product's shared isolate. Embedders may explicitly
// increase these up to MEMORY_LIMITS, or reduce every payload/element bound.
export const GATEWAY_MEMORY = Object.freeze({
  ...MEMORY_LIMITS, canonicalCacheBytes: 2 * 1024 * 1024,
  representationCacheBytes: 1024 * 1024, chunkCacheBytes: 2 * 1024 * 1024,
  canonicalCaptureBytes: 1024 * 1024, representationCaptureBytes: 256 * 1024,
  cacheEntries: 1024, refPageRows: 16, metadataEntries: 128,
});

// Reservations account for payloads, including transient legacy chunk copies,
// rather than counting jobs. A namespace has one ledger entry, even when its
// streaming fetch and immutable browse reader both own bytes. Waiters are FIFO.
export class ByteBudget {
  constructor(bytes, waitMs = 10000, contentionMs = 10000) {
    if (!Number.isSafeInteger(bytes) || bytes < 1 || bytes > 112 * 1024 * 1024 || !Number.isSafeInteger(waitMs) || waitMs < 1 || waitMs > 600000 || !Number.isSafeInteger(contentionMs) || contentionMs < 1 || contentionMs > 600000) throw new TypeError("invalid byte budget");
    this.limit = bytes; this.waitMs = waitMs; this.contentionMs = contentionMs;
    this.used = 0; this.peak = 0; this.owners = new Map(); this.waiters = [];
  }
  acquire(namespace, bytes, preempt, { signal, waitMs = this.waitMs } = {}) {
    if (bytes > this.limit || (waitMs !== 0 && this.waiters.length >= 1024)) return Promise.reject(Object.assign(new Error("isolate memory admission budget exceeded"), { status: 503 }));
    return new Promise((resolve, reject) => {
      if (signal?.aborted) { reject(Object.assign(new Error("repository job cancelled"), { status: 503 })); return; }
      const item = { namespace, bytes, preempt, resolve, reject, signal };
      item.abort = () => { clearTimeout(item.timer); this.waiters = this.waiters.filter((entry) => entry !== item); reject(Object.assign(new Error("repository job cancelled"), { status: 503 })); this.pump(); };
      signal?.addEventListener("abort", item.abort, { once: true });
      if (waitMs) item.timer = setTimeout(() => {
        this.waiters = this.waiters.filter((entry) => entry !== item);
        reject(Object.assign(new Error("isolate memory budget busy; retry later"), { status: 503 }));
        this.pump();
      }, waitMs);
      this.waiters.push(item); this.pump();
    });
  }
  pump() {
    while (this.waiters.length && this.used + this.waiters[0].bytes <= this.limit) {
      const item = this.waiters.shift(); clearTimeout(item.timer); item.signal?.removeEventListener("abort", item.abort);
      this.used += item.bytes; this.peak = Math.max(this.peak, this.used);
      const owner = this.owners.get(item.namespace) ?? new Set();
      this.owners.set(item.namespace, owner); owner.add(item);
      const release = () => {
        if (!owner.delete(item)) return;
        clearTimeout(item.contention); this.used -= item.bytes;
        if (!owner.size) this.owners.delete(item.namespace);
        this.pump();
      };
      release.resize = (bytes) => {
        if (!Number.isSafeInteger(bytes) || bytes < 0 || bytes > item.bytes) throw new TypeError("reservation can only shrink");
        this.used -= item.bytes - bytes; item.bytes = bytes; this.pump();
      };
      item.resolve(release);
    }
    for (const owner of this.owners.values()) for (const item of owner) {
      if (!this.waiters.length) { clearTimeout(item.contention); item.contention = null; }
      else if (!item.contention) item.contention = setTimeout(() => {
        item.contention = null;
        if (this.waiters.length) item.preempt();
      }, this.contentionMs);
    }
  }
}

// Conservative payload reservation: codec + caches + capture + two complete
// new chunk/copy buffers (legacy misses reserve their additional bytes) + controls/metadata + output/pins + 4 MiB headroom.
export function jobMemoryBytes(memory, codec, reads, pinBytes, pinEntries = 4096) {
  const bytes = 6 * 1024 * 1024 + memory.canonicalCacheBytes + memory.representationCacheBytes + memory.chunkCacheBytes
    + memory.canonicalCaptureBytes + memory.representationCaptureBytes + 2 * memory.chunkBytes + 2 * memory.chunkBytes
    + Math.max(codec.maxControlBytes, reads.metadataBytes) + Math.max(pinBytes, 3 * reads.jsonBytes) + Math.max(0, pinEntries - 4096) * 256 + 4 * 1024 * 1024;
  return Math.ceil(bytes / (1024 * 1024)) * 1024 * 1024;
}
