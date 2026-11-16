import { Codec } from "./codec.mjs";
const MiB = 1024 * 1024;
export const CODEC_BYTES = 6 * MiB;
export const MEMORY_LIMITS = Object.freeze({
  canonicalCacheBytes: 24 * MiB, representationCacheBytes: 8 * MiB,
  chunkCacheBytes: 16 * MiB, cacheEntries: 8192,
  canonicalCaptureBytes: 8 * MiB, representationCaptureBytes: MiB,
  chunkBytes: MiB, refPageRows: 128, metadataEntries: 256,
});
export const GATEWAY_MEMORY = Object.freeze({
  ...MEMORY_LIMITS, canonicalCacheBytes: 512 * 1024, representationCacheBytes: 256 * 1024,
  chunkCacheBytes: MiB, canonicalCaptureBytes: 256 * 1024, representationCaptureBytes: 256 * 1024,
  cacheEntries: 512, refPageRows: 16, metadataEntries: 128,
});
const busy = (message) => Object.assign(new Error(message), { status: 503 });

// Namespaces take turns, FIFO within each route class. Heavy work cannot spend
// the read headroom or a co-tenant's fourth codec arena. Active jobs are never
// preempted. Codec arenas are reused and remain charged while idle: GC latency
// cannot hide a burst of abandoned Wasm linear memories from this ledger.
export class ByteBudget {
  constructor(bytes, waitMs = 15000, contentionMs = 10000, { codecSlots = 4, readHeadroomBytes = Math.min(13 * MiB, Math.floor(bytes / 4)) } = {}) {
    if (!Number.isSafeInteger(bytes) || bytes < 1 || bytes > 112 * MiB || !Number.isSafeInteger(waitMs) || waitMs < 1 || waitMs > 600000 || !Number.isSafeInteger(contentionMs) || contentionMs < 1 || contentionMs > 600000 || !Number.isSafeInteger(codecSlots) || codecSlots < 2 || codecSlots > 16 || !Number.isSafeInteger(readHeadroomBytes) || readHeadroomBytes < 0 || readHeadroomBytes >= bytes) throw new TypeError("invalid byte budget");
    this.limit = bytes; this.waitMs = waitMs; this.codecSlots = codecSlots; this.headroom = readHeadroomBytes;
    this.used = 0; this.peak = 0; this.residentBytes = 0; this.owners = new Map(); this.waiters = []; this.arenas = []; this.lastNamespace = null;
  }
  acquire(namespace, bytes, _preempt, { signal, waitMs = this.waitMs, codec = false, heavy = false, headroom = heavy, fair = false } = {}) {
    const working = bytes - (codec ? CODEC_BYTES : 0);
    const arenaReserve = this.codecSlots * CODEC_BYTES;
    if (!Number.isSafeInteger(bytes) || working < 0 || bytes > this.limit || (fair && working > this.limit - arenaReserve - this.headroom) || (waitMs !== 0 && this.waiters.length >= 1024)) return Promise.reject(busy("isolate memory admission budget exceeded"));
    return new Promise((resolve, reject) => {
      if (signal?.aborted) { reject(busy("repository job cancelled")); return; }
      const item = { namespace, bytes, working, codec, heavy, headroom, fair, resolve, reject, signal };
      const remove = (error) => { clearTimeout(item.timer); signal?.removeEventListener("abort", item.abort); this.waiters = this.waiters.filter((entry) => entry !== item); reject(error); this.pump(); };
      item.abort = () => remove(busy("repository job cancelled"));
      signal?.addEventListener("abort", item.abort, { once: true });
      if (waitMs) item.timer = setTimeout(() => remove(busy("isolate memory budget busy; retry later")), waitMs);
      this.waiters.push(item); this.pump();
    });
  }
  fits(item, waitingHeavy) {
    const owner = this.owners.get(item.namespace) ?? new Set();
    const slots = [...owner].filter((lease) => lease.arena).length;
    const ownWorking = [...owner].reduce((sum, lease) => sum + lease.working, 0);
    if (item.fair && (ownWorking + item.working > this.limit - this.codecSlots * CODEC_BYTES - this.headroom || (item.codec && slots >= this.codecSlots - 1))) return false;
    if (item.heavy && item.codec) {
      const activeHeavy = this.arenas.filter((arena) => arena.busy && arena.lease?.heavy);
      // Keep an arena for bounded browse/control work. Once another namespace
      // waits for heavy work, incumbents cannot replenish their heavy share.
      if (activeHeavy.length >= this.codecSlots - 1) return false;
      if ([...owner].some((lease) => lease.heavy) && (waitingHeavy.size > 1 || (waitingHeavy.size === 1 && !waitingHeavy.has(item.namespace)))) return false;
    }
    const extraArena = item.codec && !this.arenas.some((arena) => !arena.busy) ? CODEC_BYTES : 0;
    if (extraArena && this.arenas.length >= this.codecSlots) return false;
    if (this.used + item.working + extraArena > this.limit) return false;
    const activeWorking = this.used - this.residentBytes;
    return !item.headroom || Math.max(this.residentBytes, this.codecSlots * CODEC_BYTES) + activeWorking + item.working <= this.limit - this.headroom;
  }
  pump() {
    for (;;) {
      const groups = new Map(), waitingHeavy = new Set();
      for (const waiting of this.waiters) {
        if (waiting.heavy) waitingHeavy.add(waiting.namespace);
        const classes = groups.get(waiting.namespace) ?? new Map(); groups.set(waiting.namespace, classes);
        const key = `${waiting.codec}:${waiting.heavy}:${waiting.headroom}`;
        if (!classes.has(key)) classes.set(key, waiting);
      }
      const namespaces = [...groups.keys()];
      const last = namespaces.indexOf(this.lastNamespace);
      if (last >= 0) namespaces.push(...namespaces.splice(0, last + 1));
      let item;
      for (const ns of namespaces) {
        // An expensive queued job cannot hide an admissible light read. FIFO
        // is maintained among equally classified jobs within a namespace.
        item = [...groups.get(ns).values()].find((entry) => this.fits(entry, waitingHeavy));
        if (item) break;
      }
      if (!item) return;
      this.waiters.splice(this.waiters.indexOf(item), 1); clearTimeout(item.timer); item.signal?.removeEventListener("abort", item.abort);
      this.lastNamespace = item.namespace;
      if (item.codec) {
        item.arena = this.arenas.find((arena) => !arena.busy);
        if (!item.arena) { item.arena = {}; this.arenas.push(item.arena); this.residentBytes += CODEC_BYTES; this.used += CODEC_BYTES; }
        item.arena.busy = true; item.arena.lease = item;
      }
      this.used += item.working; this.peak = Math.max(this.peak, this.used);
      const owner = this.owners.get(item.namespace) ?? new Set(); this.owners.set(item.namespace, owner); owner.add(item);
      const releaseArena = () => { if (item.arena) { item.arena.busy = false; item.arena.lease = null; item.arena = null; } };
      const release = () => {
        if (!owner.delete(item)) return;
        this.used -= item.working; releaseArena();
        if (!owner.size) this.owners.delete(item.namespace); this.pump();
      };
      release.codec = (module, format, limits) => {
        if (!item.arena) throw new Error("codec used without an arena reservation");
        if (item.arena.module && item.arena.module !== module) throw new TypeError("a shared byte pool requires the same codec module");
        item.arena.module = module;
        if (!item.arena.instance) item.arena.instance = new Codec(module, format, limits);
        else item.arena.instance.reset(format, limits);
        return item.arena.instance;
      };
      release.resize = (bytes) => {
        if (!Number.isSafeInteger(bytes) || bytes < 0 || bytes > item.working + (item.arena ? CODEC_BYTES : 0)) throw new TypeError("reservation can only shrink");
        if (bytes < CODEC_BYTES && item.arena) releaseArena();
        const working = bytes - (item.arena ? CODEC_BYTES : 0);
        this.used -= item.working - working; item.working = working; this.pump();
      };
      item.resolve(release);
    }
  }
}

export function jobMemoryBytes(memory, codec, reads, pinBytes, pinEntries = 4096) {
  const bytes = CODEC_BYTES + memory.canonicalCacheBytes + memory.representationCacheBytes + memory.chunkCacheBytes
    + memory.canonicalCaptureBytes + memory.representationCaptureBytes + 3 * memory.chunkBytes
    + pinBytes + Math.max(0, pinEntries - 4096) * 256 + 1.5 * MiB;
  return Math.ceil(bytes / MiB) * MiB;
}
