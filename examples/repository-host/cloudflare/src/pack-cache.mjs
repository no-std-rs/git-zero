import { ApiError, encode } from "./api.mjs";
import { requestBody, decodedBody, bodyBudget } from "./body.mjs";
import { bytes, PAGE_BYTES, sha256 } from "./storage.mjs";
import { RequestError } from "./errors.mjs";

export async function collectControls(store, owner, request, maximum) {
  let length = 0, seq = 0;
  const data = new Uint8Array(maximum), stream = decodedBody(request, 2n * BigInt(maximum) + 64n, "fetch controls");
  try {
    for await (const part of requestBody(stream, 60000, bodyBudget(request))) {
      if (part.length > maximum - length) throw new RequestError("fetch controls exceed byte limit");
      data.set(part, length); length += part.length;
    }
    // At most 1 MiB of bind copies and 48 bindings. Persist after EOF, so
    // transport fragmentation cannot force a durable write/flush per chunk.
    for (let offset = 0; offset < length;) {
      const page = [];
      while (page.length < 16 && offset < length) { const end = Math.min(length, offset + PAGE_BYTES); page.push([owner.id, seq++, data.slice(offset, end).buffer]); offset = end; }
      try { store.exec(`INSERT INTO job_controls(job,seq,data) VALUES${page.map(() => "(?,?,?)").join(",")}`, ...page.flat()); }
      finally { for (const row of page) row[2].transfer(0); }
    }
  } finally { data.buffer.transfer(0); }
}

const noCache = { get() {}, put() {} };
export class PackCache {
  constructor(store, env) { this.store = store; this.env = env; this.builds = new Map(); this.pins = new Map(); this.metrics = { builds: 0, hits: 0, served: 0, generatedBytes: 0 }; store.exec("UPDATE fetch_cache SET state='failed' WHERE state='building'"); }
  async key(fetch) {
    // Shared failures can be reported in-band only with negotiated sideband.
    if (!fetch.band || !fetch.cacheable()) return null;
    let result = await sha256(encode(JSON.stringify(["git-zero-wire-pack-v1", fetch.store.snapshot.epoch, fetch.store.snapshot.generation, fetch.version, fetch.parsedFlags, this.env.WANT_POLICY, this.env.CODEC_LIMITS, this.env.READ_SCRATCH_BYTES])));
    let after = "", page = 0;
    for (;;) {
      const rows = fetch.store.exec("SELECT oid FROM fetch_wants WHERE oid>? ORDER BY oid LIMIT 128", after);
      if (!rows.length) break;
      result = await sha256(encode(JSON.stringify([result, page++, rows.map((row) => row.oid)])));
      after = rows.at(-1).oid;
    }
    return result;
  }

  maximum(fetch) {
    const generation = fetch.store.snapshot.generation;
    if (this.bound?.generation === generation) return this.bound.bytes;
    // One aggregate per snapshot generation, no graph walk or object reads.
    // Include duplicate/unreachable published representations conservatively;
    // each output is at most its stored stream or a stored-block canonical
    // fallback, plus header/base/framing allowance. Never under-reserve.
    const row = fetch.store.exec("SELECT COUNT(*) n,COALESCE(SUM(MAX(o.end_offset-o.compressed_offset,o.size+5*((o.size+65534)/65535)+11)+64),0) bytes FROM stage_objects o JOIN stages s ON s.id=o.stage WHERE s.phase='published' AND o.verified=1")[0];
    const pack = 12 + fetch.codec.idBytes + row.bytes;
    const bytes = pack + 16 * (Math.ceil(pack / 512) + 6 * row.n + 128);
    this.bound = { generation, bytes }; return bytes;
  }

  entry(key) {
    const active = this.builds.get(key); if (active) { this.metrics.hits++; return active; }
    const row = this.store.exec("SELECT * FROM fetch_cache WHERE key=? AND state='complete'", key)[0];
    if (!row) return null;
    this.metrics.hits++; return { key, cache: true, complete: true, row };
  }
  begin(key) {
    const entry = { key, cache: null, complete: false, error: null, lastOutput: Date.now() }; this.notify(entry);
    this.builds.set(key, entry); return entry;
  }
  notify(entry) { entry.wake?.(); entry.changed = new Promise((resolve) => { entry.wake = resolve; }); }
  async wait(entry, signal) {
    if (signal.aborted) throw new RequestError("repository job cancelled");
    let timer, abort;
    try {
      await Promise.race([entry.changed, new Promise((_, reject) => {
        abort = () => reject(new RequestError("repository job cancelled")); signal.addEventListener("abort", abort, { once: true });
        timer = setTimeout(() => reject(new ApiError("cached pack preparation timed out; retry", 503)), this.env.PACK_CACHE_BUILD_MS + this.env.FETCH_PREPARE_MS + this.env.MEMORY_WAIT_MS);
      })]);
    } finally { clearTimeout(timer); signal.removeEventListener("abort", abort); }
  }
  async decision(entry, signal) { while (entry.cache === null && !entry.error) await this.wait(entry, signal); if (entry.error) return false; return entry.cache; }
  claim(entry, generation, maximum) {
    const reserved = this.store.exec("SELECT COALESCE(SUM(reserved),0) n,COUNT(*) count FROM fetch_cache")[0];
    if (!Number.isSafeInteger(maximum) || maximum > this.env.PACK_CACHE_BYTES || maximum > this.env.PACK_CACHE_TOTAL_BYTES - reserved.n || reserved.count >= this.env.PACK_CACHE_ENTRIES) return false;
    if (this.store.exec("SELECT 1 FROM fetch_cache WHERE key=?", entry.key).length) return false;
    this.store.exec("INSERT INTO fetch_cache(key,generation,state,reserved,created) VALUES(?,?,'building',?,?)", entry.key, generation, maximum, Date.now());
    this.store.exec("UPDATE fetch_cache_metrics SET builds=builds+1 WHERE id=1"); entry.maximum = maximum; entry.cache = true; this.metrics.builds++; this.notify(entry); return true;
  }
  bypass(entry) { entry.cache = false; this.notify(entry); this.builds.delete(entry.key); }
  fail(entry, error) {
    clearTimeout(entry.orphanTimer);
    entry.error = error; this.store.exec("UPDATE fetch_cache SET state='failed' WHERE key=?", entry.key);
    this.notify(entry); this.builds.delete(entry.key);
  }
  watch(entry, cancel) { entry.cancel = cancel; this.orphan(entry); }
  orphan(entry) {
    clearTimeout(entry.orphanTimer);
    if (!entry.cancel || entry.complete || entry.error || this.pins.has(entry.key)) return;
    const window = this.env.RESPONSE_RATE_WINDOW_MS ?? 30000;
    entry.orphanTimer = setTimeout(() => {
      if (this.pins.has(entry.key) || entry.complete || entry.error) return;
      if (entry.cache === false || Date.now() - entry.lastOutput >= window) entry.cancel();
      else this.orphan(entry);
    }, Math.max(1, entry.lastOutput + window - Date.now()));
  }
  pin(entry) { clearTimeout(entry.orphanTimer); this.pins.set(entry.key, (this.pins.get(entry.key) ?? 0) + 1); let released = false; return () => { if (released) return; released = true; const count = this.pins.get(entry.key) - 1; if (count) this.pins.set(entry.key, count); else {
      this.pins.delete(entry.key);
      this.orphan(entry);
      const alarm = this.store.ctx.storage.setAlarm(Date.now() + 1000).catch(() => {}); this.store.ctx.waitUntil?.(alarm);
    } }; }
  async produce(entry, fetch, signal) {
    let buffer = new Uint8Array(this.env.PACK_CACHE_CHUNK_BYTES), used = 0, seq = 0, total = 0;
    // Credit grows with useful output. A 300 MiB pack may take 4800 s at
    // 64 KiB/s; only stalls/below-rate builds are stopped, never its age alone.
    const started = Date.now(), rate = this.env.PACK_CACHE_MINIMUM_RATE ?? 65536;
    const flush = async () => {
      if (!used) return;
      if (signal.aborted) throw new RequestError("repository job cancelled");
      if (Date.now() - started > this.env.PACK_CACHE_BUILD_MS + total / rate * 1000) throw new ApiError("pack cache generation deadline exceeded", 504);
      if (total + used > entry.maximum) throw new ApiError("pack cache size reservation exceeded", 413);
      const part = buffer.subarray(0, used), digest = await sha256(part);
      const key = `${this.env.KEY_PREFIX}repositories/${this.store.namespace}/fetch-cache/${entry.key}/${seq}`;
      this.store.exec("INSERT INTO fetch_cache_chunks(cache,seq,key,length,sha) VALUES(?,?,?,?,?)", entry.key, seq, key, used, digest);
      await this.store.put(key, part);
      this.store.exec("UPDATE fetch_cache_chunks SET ready=1 WHERE cache=? AND seq=?", entry.key, seq);
      total += used; seq++; used = 0; entry.lastOutput = Date.now(); this.notify(entry);
    };
    try {
      for await (const part of fetch.response()) {
        for (let at = 0; at < part.length;) {
          const count = Math.min(part.length - at, buffer.length - used); buffer.set(part.subarray(at, at + count), used); used += count; at += count;
          if (used === buffer.length) await flush();
        }
      }
      await flush(); buffer = null;
      this.store.exec("UPDATE fetch_cache SET state='complete',bytes=?,reserved=? WHERE key=?", total, total, entry.key);
      this.store.exec("UPDATE fetch_cache_metrics SET generated_bytes=generated_bytes+? WHERE id=1", total); entry.complete = true; clearTimeout(entry.orphanTimer); this.metrics.generatedBytes += total; this.notify(entry); this.builds.delete(entry.key);
    } catch (error) { this.fail(entry, error); throw error; }
    finally { await fetch.cleanup(); }
  }
  async first(entry, signal) {
    while (!entry.complete && !entry.error && !this.store.exec("SELECT 1 FROM fetch_cache_chunks WHERE cache=? AND ready=1 LIMIT 1", entry.key).length) await this.wait(entry, signal);
    return !entry.error;
  }
  async *read(entry, signal) {
    this.metrics.served++; let seq = 0;
    // Never expose a partial pkt-line: an eventual fatal sideband packet must
    // start at a framing boundary even if an R2 chunk ends inside pack data.
    const packet = entry.band ? new Uint8Array(65536) : null; let used = 0, expected = 4;
    try {
    for (;;) {
      if (signal.aborted) throw new RequestError("repository job cancelled");
      const row = this.store.exec("SELECT * FROM fetch_cache_chunks WHERE cache=? AND seq=? AND ready=1", entry.key, seq)[0];
      if (row) {
        const data = await this.store.get(row, noCache); seq++;
        if (!packet) { for (let at = 0; at < data.length; at += PAGE_BYTES) { if (signal.aborted) throw new RequestError("repository job cancelled"); yield data.subarray(at, at + PAGE_BYTES); } }
        else for (let at = 0; at < data.length;) {
          if (signal.aborted) throw new RequestError("repository job cancelled");
          const count = Math.min(expected - used, data.length - at); packet.set(data.subarray(at, at + count), used); at += count; used += count;
          if (used === 4 && expected === 4) {
            const header = new TextDecoder().decode(packet.subarray(0, 4));
            if (!/^[0-9a-f]{4}$/.test(header)) throw new Error("cached pack framing invalid");
            expected = Math.max(4, Number.parseInt(header, 16));
          }
          if (used === expected) { yield packet.slice(0, used); used = 0; expected = 4; }
        }
      } else {
        if (entry.error) throw entry.error;
        if (entry.complete) { if (used) throw new Error("cached pack frame truncated"); return; }
        await this.wait(entry, signal);
      }
    }
    } catch (error) {
      if (!packet || signal.aborted) throw error;
      const fatal = encode("\x03git-zero: shared pack generation failed; retry fetch\n");
      const framed = new Uint8Array(fatal.length + 4); framed.set(encode(framed.length.toString(16).padStart(4, "0"))); framed.set(fatal, 4); yield framed;
    }
  }
  async reclaim(currentGeneration) {
    const candidates = this.store.exec("SELECT key FROM fetch_cache WHERE generation<? OR state IN ('failed','evicting') ORDER BY created LIMIT 8", currentGeneration);
    for (const item of candidates) {
      if (this.pins.has(item.key) || this.builds.has(item.key)) continue;
      this.store.exec("UPDATE fetch_cache SET state='evicting' WHERE key=?", item.key);
      const chunks = this.store.exec("SELECT seq,key FROM fetch_cache_chunks WHERE cache=? ORDER BY seq LIMIT 64", item.key);
      if (chunks.length) {
        await this.store.delete(chunks.map((row) => row.key));
        for (const row of chunks) this.store.exec("DELETE FROM fetch_cache_chunks WHERE cache=? AND seq=?", item.key, row.seq);
      }
      if (!this.store.exec("SELECT 1 FROM fetch_cache_chunks WHERE cache=? LIMIT 1", item.key).length) this.store.exec("DELETE FROM fetch_cache WHERE key=?", item.key);
      return true;
    }
    return false;
  }
}
