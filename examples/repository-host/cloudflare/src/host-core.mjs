import { receiveControls } from "./receive-controls.mjs";
import { PackCache, collectControls } from "./pack-cache.mjs";
import { JobGate, jobStore, snapshot } from "./jobs.mjs";
import { Codec, CodecError } from "./codec.mjs";
import { Intake } from "./intake.mjs";
import { ObjectSource } from "./source.mjs";
import { Store, assertStoragePrefix } from "./storage.mjs";
import { bytes } from "./storage.mjs";
import { publish } from "./publication.mjs";
import { refusal, bindBodyBudget } from "./body.mjs";
import { FetchJob, WantError } from "./fetch.mjs";
import { streamResponse } from "./response.mjs";
import { RequestError, isStorageFailure } from "./errors.mjs";

import { Lifecycle, creationSettings } from "./lifecycle.mjs";
import { advanceIndex } from "./reachability.mjs";
import { ReadApi } from "./browse.mjs";
import { changes } from "./changes.mjs";
import { PolicyError, policyRoute } from "./policy.mjs";
import { ApiError, apiRoute, READ_ROUTES } from "./api.mjs";
import { REPOSITORY, authorize } from "./access.mjs";
const UUID = /^[0-9a-f-]{36}$/;
const json = (body, status = 200) => Response.json(body, { status, headers: { "Cache-Control": "no-store" } });
const encode = (value) => new TextEncoder().encode(value);
const fromHex = (value) => Uint8Array.from(value.match(/../g), (pair) => Number.parseInt(pair, 16));
// Advertise only host-backed receive features. Push options are staged until
// publication; this host has no application hooks that consume them.
const RECEIVE_CAPABILITIES = 255;

function testFormat(request, fallback = 0) {
  const format = Number(request.headers.get("x-test-format") ?? fallback);
  if (format !== 0 && format !== 1) throw new RequestError("unsupported object format");
  return format;
}

export class RepositoryCore {
  constructor(ctx, env, module) {
    this.ctx = ctx; this.env = env; this.module = module; this.maxRequests = env.MAX_REQUESTS; this.pendingRequests = 0; this.store = null;
    this.queue = Promise.resolve(); this.opening = null; this.recovery = Promise.resolve(); this.coldRecovery = Promise.resolve(); this.streamDone = null; this.epoch = crypto.randomUUID();
    this.gate = new JobGate(env.MAX_CONCURRENT ?? 32, env.MAX_PENDING ?? 64, env.ADMISSION_WAIT_MS ?? 15000);
    this.jobs = new Map(); this.deleting = false; this.deletions = Promise.resolve();
  }
  async fetch(request) {
    // Capture authorization at arrival, independently of scratch ownership,
    // recovery, alarm jobs and the lifetime of an earlier response stream.
    const namespace = request.headers.get("x-git-zero-namespace");
    const access = await authorize(request, this.env, namespace);
    if (access instanceof Response) return refusal(request, this.env.AUTH_MODE === "gateway" ? access : json({ error: "unknown route" }, 404), this.env, access.status === 200);
    if (this.deleting && apiRoute(request) !== "repository") return refusal(request, json({ error: "unknown route" }, 404), this.env, true);
    if (this.pendingRequests >= this.maxRequests && !(request.method === "DELETE" && apiRoute(request) === "repository")) {
      const response = json({ error: "repository host busy; retry later" }, 503);
      response.headers.set("Retry-After", "1"); return refusal(request, response, this.env, true);
    }
    const owner = { destructive: request.method === "DELETE" && apiRoute(request) === "repository", id: crypto.randomUUID(), done: null, abort: new AbortController(), preemptible: request.method === "GET", stages: new Set(), temporaries: new Set() };
    let finish; owner.finished = new Promise((resolve) => { finish = resolve; });
    this.jobs.set(owner.id, owner); this.pendingRequests++;
    this.queue = Promise.allSettled([...this.jobs.values()].map((job) => job.finished)).then(() => {});
    this.resources(owner, namespace);
    bindBodyBudget(request, { ...this.env, BODY_SIGNAL: owner.abort.signal });
    let releaseSlot, response;
    const complete = async () => {
      await Promise.resolve(owner.done).catch(() => {});
      owner.release?.(); for (const finalizer of owner.finalizers ?? []) await finalizer(); releaseSlot?.(); this.jobs.delete(owner.id); this.pendingRequests--; finish();
    };
    try {
      if (!owner.destructive) { await this.coldRecovery; releaseSlot = await this.gate.enter(owner.abort.signal); }
      response = await this.handle(request, access, owner);
    } catch (error) {
      response = json({ error: this.deleting && !owner.destructive ? "unknown route" : String(error.message ?? error) }, this.deleting && !owner.destructive ? 404 : error.status ?? 500);
      if (response.status === 503) response.headers.set("Retry-After", "1");
    } finally { const completion = complete(); this.ctx.waitUntil?.(completion); completion.catch(() => {}); }
    if (owner.drain) {
      const draining = this.env.DRAIN_STATE ??= { active: 0 };
      if (draining.active >= 2) { await owner.cancelDrain?.(); return response; }
      draining.active++;
      try { await owner.drain(); } finally { draining.active--; }
    }
    return refusal(request, response, this.env, true);
  }
  resources(owner, namespace) {
    owner.finalizers = new Set();
    owner.releaseMemory = () => { owner.release?.(); owner.release = null; owner.leaseState = null; };
    const acquire = (bytes, claim) => this.env.BYTE_BUDGET?.acquire(namespace, bytes, () => { const job = claim.owner; if (job.preemptible && this.env.AUTH_MODE !== "token") job.abort.abort(new Error("response preempted for memory contention")); }, { signal: claim.owner.abort.signal, waitMs: this.env.AUTH_MODE === "token" ? 0 : undefined });
    owner.reserve = async (bytes = this.env.JOB_MEMORY_BYTES) => {
      if (!owner.release && this.env.BYTE_BUDGET) { const claim = { owner }; owner.release = await acquire(bytes, claim); owner.leaseState = claim; }
    };
    owner.extra = async (bytes) => await acquire(bytes, { owner });
  }
  promote(owner, fetch, operation) {
    if (this.deleting || owner.abort.signal.aborted) throw new RequestError("repository job cancelled");
    const builder = { id: crypto.randomUUID(), internal: true, snapshot: owner.snapshot, stages: new Set(), temporaries: new Set([fetch.temporary]), abort: new AbortController(), preemptible: true };
    this.resources(builder, this.store.namespace); builder.release = owner.release; builder.leaseState = owner.leaseState;
    if (builder.leaseState) builder.leaseState.owner = builder;
    owner.release = null; owner.leaseState = null; owner.temporaries.delete(fetch.temporary);
    fetch.store.env.JOB_SIGNAL = builder.abort.signal; fetch.store.env.JOB_OWNER = builder;
    let finish; builder.finished = new Promise((resolve) => { finish = resolve; }); this.jobs.set(builder.id, builder);
    this.queue = Promise.allSettled([...this.jobs.values()].map((job) => job.finished)).then(() => {});
    const running = operation(builder).catch((error) => { console.error(JSON.stringify({ event: "pack-cache-build-failed", error: String(error.message ?? error) })); }).finally(() => { builder.release?.(); this.jobs.delete(builder.id); finish(); });
    this.ctx.waitUntil?.(running); return running;
  }
  retire(owner) {
    const work = this.deletions.then(async () => {
      this.deleting = true;
      for (const job of this.jobs.values()) if (!job.destructive) job.abort.abort(new Error("repository deleted; in-flight operation cancelled"));
      const waiting = [...this.jobs.values()].some((job) => job !== owner && !job.destructive);
      let response;
      if (!this.lifecycle.state) response = await this.lifecycle.beginDelete({ defer: waiting });
      else if (this.lifecycle.state.state === "deleted") response = json({ state: "deleted", complete: true });
      else if (waiting) response = json({ state: "deleting", complete: false, waitingForJobs: true }, 202);
      else response = await this.lifecycle.purge();
      if (this.lifecycle.state.state === "deleted") { this.store = null; this.opening = null; }
      return response;
    });
    this.deletions = work.catch(() => {}); return work;
  }
  alarm() {
    const coldStart = !this.store;
    const maintenance = { id: crypto.randomUUID(), internal: true, stages: new Set(), temporaries: new Set(), abort: new AbortController() };
    let finished; maintenance.finished = new Promise((resolve) => { finished = resolve; }); this.jobs.set(maintenance.id, maintenance);
    const job = this.recovery.then(async () => {
      if (await this.ctx.storage.get("host.deletion.v1")) return;
      if (!this.ctx.storage.sql.exec("SELECT 1 FROM sqlite_master WHERE type='table' AND name='host_identity'").toArray().length) return;
      const identity = this.ctx.storage.sql.exec("SELECT namespace FROM host_identity WHERE id=1").toArray()[0]; if (!identity) return;
      const cold = !this.store; this.store ??= new Store(this.ctx, this.env, identity.namespace);
      if (cold || (this.store.recoveryDue ?? Infinity) <= Date.now()) {
        await this.store.recover(() => [...this.jobs.values()].flatMap((owner) => [owner.id, ...owner.stages, ...owner.temporaries]));
      }
      if (maintenance.abort.signal.aborted) return;
      this.packCache ??= new PackCache(this.store, this.env);
      const repository = this.store.exec("SELECT generation FROM repository WHERE id=1")[0];
      let nextAlarm = this.store.recoveryDue ?? Infinity;
      if (repository) {
        const reclaiming = await this.packCache.reclaim(repository.generation);
        if (maintenance.abort.signal.aborted) return;
        if (reclaiming) nextAlarm = Math.min(nextAlarm, Date.now() + 1000);
        const progress = advanceIndex(this.store, repository.generation, this.env.READ_LIMITS);
        if (progress.phase !== 2 && progress.objects < this.env.READ_LIMITS.visibleObjects && progress.edges < this.env.READ_LIMITS.edges) nextAlarm = Math.min(nextAlarm, Date.now() + 1000);
      }
      if (Number.isFinite(nextAlarm)) await this.ctx.storage.setAlarm(Math.max(Date.now() + 1, nextAlarm));
    });
    job.finally(() => { this.jobs.delete(maintenance.id); finished(); }).catch(() => {});
    this.recovery = job.catch(() => {}); if (coldStart) this.coldRecovery = this.recovery; this.queue = Promise.allSettled([this.queue, job]).then(() => {}); return job;
  }
  stream(generator, type, length, owner) {
    const result = streamResponse(generator, type, this.env.RESPONSE_IDLE_MS, length, this.env.RESPONSE_TOTAL_MS, { minimumBytesPerSecond: this.env.RESPONSE_MINIMUM_RATE, throughputWindowMs: this.env.RESPONSE_RATE_WINDOW_MS, signal: owner?.abort?.signal, onStop: (error) => owner?.abort?.abort(error ?? new Error("response cancelled")) });
    if (owner) owner.done = result.done; else this.streamDone = result.done;
    return result.response;
  }
  codecForUpload(format) {
    return new Codec(this.module, format, { ...this.env.CODEC_LIMITS, maxTemporaryBytes: Math.min(this.env.CODEC_LIMITS.maxTemporaryBytes, this.env.READ_SCRATCH_BYTES) });
  }
  async upload(request, owner, format, stream) {
    const scoped = jobStore(this.store, owner, { read: true, fetch: true }); owner.temporaries.add(owner.id);
    owner.preemptible = true;
    let fetch, tables = false, shared = false, ownedEntry;
    const discardParsed = () => { fetch?.detach(); owner.releaseMemory(); };
    try {
      await owner.reserve(this.env.CODEC_LIMITS.maxControlBytes + 256 * 1024);
      await collectControls(this.store, owner, request, this.env.CODEC_LIMITS.maxControlBytes);
      owner.releaseMemory();
      const useCache = this.env.PACK_CACHE_ENABLED && this.env.PACK_CACHE_BYTES;
      await owner.reserve(useCache ? this.env.METADATA_MEMORY_BYTES : this.env.JOB_MEMORY_BYTES);
      scoped.createFetchTables(); tables = true;
      fetch = this.parsedUpload(scoped, request, format); await fetch.decodeSaved();
      if (fetch.command !== 2) { owner.release?.resize?.(this.env.METADATA_MEMORY_BYTES); return stream(fetch.response(), "application/x-git-upload-pack-result"); }
      if (!useCache) {
        await fetch.selection(); await fetch.prepare(); owner.preparedFallbacks = fetch.fallbacks ?? 0;
        return stream(fetch.response(), "application/x-git-upload-pack-result");
      }
      const cache = this.packCache ??= new PackCache(this.store, this.env);
      const key = this.env.PACK_CACHE_ENABLED && this.env.PACK_CACHE_BYTES ? await cache.key(fetch) : null;
      if (owner.abort.signal.aborted) throw new RequestError("repository job cancelled");
      if (key) {
        const entry = cache.entry(key);
        if (entry) {
          const unpin = cache.pin(entry); owner.finalizers.add(unpin); discardParsed();
          if (await cache.decision(entry, owner.abort.signal)) {
            scoped.dropFetchTables(); tables = false;
            await cache.first(entry, owner.abort.signal); await owner.reserve(this.env.CACHE_READER_MEMORY_BYTES);
            owner.preemptible = true; return stream(cache.read(entry, owner.abort.signal), "application/x-git-upload-pack-result");
          }
        } else if (cache.builds.size < 2) {
          const building = ownedEntry = cache.begin(key); discardParsed(); await owner.reserve();
          fetch.replay(this.codecForUpload(format));
          try { await fetch.selection(); await fetch.prepare(); owner.preparedFallbacks = fetch.fallbacks ?? 0; }
          catch (error) { cache.fail(building, error); throw error; }
          if (cache.claim(building, owner.snapshot.generation, fetch.maximumWireBytes())) {
            const unpin = cache.pin(building); owner.finalizers.add(unpin);
            this.promote(owner, fetch, (builder) => cache.produce(building, fetch, builder.abort.signal)); shared = true; tables = false;
            await cache.first(building, owner.abort.signal); await owner.reserve(this.env.CACHE_READER_MEMORY_BYTES);
            owner.preemptible = true; return stream(cache.read(building, owner.abort.signal), "application/x-git-upload-pack-result");
          }
          cache.bypass(building);
          owner.preemptible = true; return stream(fetch.response(), "application/x-git-upload-pack-result");
        }
      }
      discardParsed(); await owner.reserve(); fetch.replay(this.codecForUpload(format));
      await fetch.selection(); await fetch.prepare(); owner.preparedFallbacks = fetch.fallbacks ?? 0; owner.preemptible = true;
      return stream(fetch.response(), "application/x-git-upload-pack-result");
    } catch (error) {
      if (ownedEntry && this.packCache.builds.get(ownedEntry.key) === ownedEntry && !shared) this.packCache.fail(ownedEntry, error);
      if (!shared) { if (fetch && tables) await fetch.cleanup(); else if (tables) scoped.dropFetchTables(); else this.store.exec("DELETE FROM job_controls WHERE job=?", owner.id); }
      if (error instanceof WantError) { const message = `ERR ${error.message}\n`; return new Response((encode(message).length + 4).toString(16).padStart(4, "0") + message, { headers: { "Content-Type": "application/x-git-upload-pack-result", "Cache-Control": "no-store" } }); }
      throw error;
    }
  }
  parsedUpload(scoped, request, format) {
    const codec = this.codecForUpload(format), version = codec.uploadVersion(request.headers.get("git-protocol")); return new FetchJob(scoped, codec, version);
  }
  async advertisement(generator, type, owner, stream) {
    const parts = []; let length = 0;
    for (;;) {
      const next = await generator.next();
      if (next.done) break;
      if (length + next.value.length > 256 * 1024) {
        const prefix = next.value;
        async function* remaining() { try { for (const part of parts) yield part; yield prefix; yield* generator; } finally { await generator.return(); } }
        return stream(remaining(), type);
      }
      parts.push(next.value); length += next.value.length;
    }
    const data = new Uint8Array(length); let offset = 0;
    for (const part of parts) { data.set(part, offset); offset += part.length; }
    parts.length = 0; generator = null;
    owner.release?.resize?.(Math.max(65536, 2 * length));
    async function* body() { yield data; }
    return stream(body(), type);
  }
  async *advertise(codec, store = this.store) {
    yield codec.advertStart();
    let after = new Uint8Array();
    for (;;) {
      const refs = store.exec("SELECT name,oid FROM refs WHERE name>? ORDER BY name LIMIT ?", after.slice().buffer, this.env.MEMORY.refPageRows);
      if (!refs.length) break;
      for (const ref of refs) { after = bytes(ref.name); yield codec.advertRef(fromHex(ref.oid), after); }
    }
    yield codec.advertFinish();
  }
  async *report(codec, selected, outcomes, failure) {
    yield codec.statusStart(selected, failure ? encode(typeof failure === "string" ? failure : "invalid pack or object graph") : new Uint8Array());
    for (const outcome of outcomes) yield codec.statusRef(bytes(outcome.name), outcome.error ? encode(outcome.error) : new Uint8Array());
    yield codec.statusFinish();
  }
  async reportResponse(codec, selected, outcomes, failure) {
    const parts = []; let length = 0;
    for await (const part of this.report(codec, selected, outcomes, failure)) { parts.push(part); length += part.length; }
    const data = new Uint8Array(length); let at = 0;
    for (const part of parts) { data.set(part, at); at += part.length; }
    return new Response(data, { headers: { "Content-Type": "application/x-git-receive-pack-result", "Cache-Control": "no-store" } });
  }
  async receive(request, codec, actor, stream, owner) {
    const store = jobStore(this.store, owner);
    const stage = store.stage(codec.format); owner.stages.add(stage);
    try {
      await new Intake(store, codec, stage).run(request);
    } catch (error) {
      if (isStorageFailure(error)) throw error;
      owner.drain = error.drain; owner.cancelDrain = error.cancelDrain;
      const selected = error.selected ?? 0;
      if (!selected) return json({ error: "invalid receive request" }, 400);
      const names = error.commands ?? this.store.exec("SELECT name FROM stage_commands WHERE stage=? ORDER BY seq", stage);
      // Intake has already reclaimed the stage or scheduled its recovery.
      // Report the original validation failure without retrying cleanup here.
      const reason = error instanceof PolicyError || (error instanceof RequestError && /^request body /.test(error.message)) ? error.message : null;
      const outcomes = names.map((row) => ({ ...row, error: reason ?? "unpacker error" }));
      return this.reportResponse(codec, selected, outcomes, reason ?? true);
    }
    if (this.deleting || owner.abort.signal.aborted) throw new RequestError("repository is deleting");
    const outcomes = await publish(store, stage, codec, actor);
    try { await this.ctx.storage.setAlarm(Date.now() + 1000); }
    catch (error) { console.error(JSON.stringify({ event: "index-alarm-deferred", error: String(error.message ?? error) })); }
    const publication = this.store.exec("SELECT selected,phase FROM stages WHERE id=?", stage)[0];
    if (publication.phase !== "published") {
      try { await this.store.abandon(stage); }
      catch (error) {
        // Ref outcomes are already durable. Abandon keeps the failed stage's
        // ownership records and schedules recovery; cleanup cannot undo them.
        console.error(JSON.stringify({ event: "cleanup-deferred", stage, error: String(error.message ?? error) }));
      }
    }
    return this.reportResponse(codec, publication.selected, outcomes, false);
  }
  async *uploadAdvertisement(codec, version, store = this.store) {
    yield codec.uploadAdvert(version, true);
    if (version !== 2) yield* new FetchJob(store, codec, version).references();
  }
  async handle(request, access, owner = {}) {
    const stream = (generator, type, length) => this.stream(generator, type, length, owner);
    try {
      if (owner.abort?.signal.aborted || (this.deleting && apiRoute(request) !== "repository")) return json({ error: "unknown route" }, 404);
      const namespace = request.headers.get("x-git-zero-namespace");
      const path = new URL(request.url).pathname.split("/");
      const routed = path[1] === "_test" ? path[2] : path[1]?.endsWith(".git") ? path[1].slice(0, -4) : null;
      if (routed !== namespace) return json({ error: "unknown route" }, 404);
      if (!REPOSITORY.test(namespace ?? "")) return json({ error: "invalid repository" }, 400);
      if (this.store && this.store.namespace !== namespace) return json({ error: "unknown route" }, 404);
      if (!this.store && this.ctx.storage.sql.exec("SELECT 1 FROM sqlite_master WHERE type='table' AND name='host_identity'").toArray().length) {
        const identity = this.ctx.storage.sql.exec("SELECT namespace FROM host_identity WHERE id=1").toArray()[0];
        if (identity && identity.namespace !== namespace) return json({ error: "unknown route" }, 404);
      }
      assertStoragePrefix(this.ctx, this.env.KEY_PREFIX);
      if (this.lifecycle && this.lifecycle.namespace !== namespace) return json({ error: "unknown route" }, 404);
      this.lifecycle ??= new Lifecycle(this.ctx, this.env, namespace);
      await this.lifecycle.load();
      if (this.lifecycle.state) {
        if (request.method === "DELETE" && apiRoute(request) === "repository") return this.retire(owner);
        const response = await this.lifecycle.retired(request);
        if (this.lifecycle.state.state === "deleted") this.store = null;
        return response;
      }
      const url = new URL(request.url), parts = url.pathname.split("/"), route = apiRoute(request);
      const tables = this.ctx.storage.sql.exec("SELECT name FROM sqlite_master WHERE type='table' AND name IN ('repository','host_identity','host_storage_config')").toArray();
      const exists = tables.some((row) => row.name === "repository") && this.ctx.storage.sql.exec("SELECT 1 FROM repository WHERE id=1").toArray().length;
      let creation;
      if (!exists) {
        if (route === "repository" && request.method === "DELETE" && tables.length) {
          return this.retire(owner);
        }
        if (route === "repository" && request.method === "PUT") creation = await creationSettings(request);
        else if (!(parts[1] === "_test" && this.env.TEST_MODE === "true") && !(parts[1] !== "_test" && !route && this.env.AUTH_MODE === "token" && this.env.IMPLICIT_CREATE !== "false")) return json({ error: "unknown route" }, 404);
      }
      if (!this.store) {
        this.opening ??= (async () => { try { const store = new Store(this.ctx, this.env, namespace); await store.recover(); this.store = store; } catch (error) { this.opening = null; throw error; } })();
        await this.opening;
      }
      if (this.store.namespace !== namespace) throw new Error("repository identity differs");
      for (const field of ["r2FenceGets", "r2Failures", "r2Retries"]) { this.store.usage[field] += this.lifecycle.usage[field]; this.lifecycle.usage[field] = 0; }
      if (apiRoute(request) === "repository") {
        if (request.method === "DELETE") return this.retire(owner);
        const response = await this.lifecycle.handle(request, this.store, creation);
        if (this.lifecycle.state?.state === "deleted") this.store = null;
        return response;
      }
      owner.snapshot ??= snapshot(this.store);
      if (apiRoute(request) === "policy") return await policyRoute(this.store, request);
      if (READ_ROUTES.has(apiRoute(request))) {
        const state = this.store.exec("SELECT format FROM repository WHERE id=1")[0];
        if (!state) return json({ error: "unknown route" }, 404);
        if (apiRoute(request) === "changes") { await owner.reserve?.(3 * this.env.READ_LIMITS.jsonBytes); return await changes(this.store, request); }
        await owner.reserve?.(apiRoute(request) === "refs" ? 3 * this.env.READ_LIMITS.jsonBytes : this.env.JOB_MEMORY_BYTES);
        owner.preemptible = true;
        return await new ReadApi(jobStore(this.store, owner, { read: true }), this.module, state.format, this.env.READ_LIMITS).handle(apiRoute(request), request, stream);
      }
      if (apiRoute(request)) return json({ error: "unknown route" }, 404);
      if (parts[1] !== "_test") {
        const format = this.store.exec("SELECT format FROM repository WHERE id=1")[0]?.format ?? Number(this.env.REPO_FORMAT ?? 0);
        if ((this.env.AUTH_MODE === "gateway" || this.env.IMPLICIT_CREATE === "false") && !this.store.exec("SELECT id FROM repository WHERE id=1").length) return json({ error: "unknown route" }, 404);
        this.store.initialize(format);
        owner.snapshot ??= snapshot(this.store);
        const contentType = request.headers.get("content-type")?.split(";")[0].trim().toLowerCase();
        if (request.method === "POST" && parts[2] === "git-upload-pack" && parts.length === 3 && contentType === "application/x-git-upload-pack-request") return await this.upload(request, owner, format, stream);
        if (request.method === "POST" && parts[2] === "git-receive-pack" && parts.length === 3 && contentType === "application/x-git-receive-pack-request") {
          await owner.reserve(this.env.METADATA_MEMORY_BYTES);
          const replay = await receiveControls(jobStore(this.store, owner), this.module, format, request, owner, RECEIVE_CAPABILITIES);
          owner.releaseMemory(); await owner.reserve();
          return await this.receive(replay, new Codec(this.module, format, { ...this.env.CODEC_LIMITS, capabilities: RECEIVE_CAPABILITIES }), access.actor, stream, owner);
        }
        await owner.reserve?.(request.method === "GET" ? this.env.METADATA_MEMORY_BYTES : this.env.JOB_MEMORY_BYTES);
        let codec = new Codec(this.module, format, { ...this.env.CODEC_LIMITS, capabilities: RECEIVE_CAPABILITIES });
        if (request.method === "GET" && parts[2] === "info" && parts[3] === "refs" && url.searchParams.get("service") === "git-receive-pack") { owner.preemptible = true; const generator = this.advertise(codec, jobStore(this.store, owner, { read: true })); codec = null; return this.advertisement(generator, "application/x-git-receive-pack-advertisement", owner, stream); }
        const version = codec.uploadVersion(request.headers.get("git-protocol"));
        if (request.method === "GET" && parts[2] === "info" && parts[3] === "refs" && url.searchParams.get("service") === "git-upload-pack") { owner.preemptible = true; const generator = this.uploadAdvertisement(codec, version, jobStore(this.store, owner, { read: true })); codec = null; return this.advertisement(generator, "application/x-git-upload-pack-advertisement", owner, stream); }
        return json({ error: "unknown Git route" }, 404);
      }
      if (this.env.TEST_MODE !== "true") return json({ error: "unknown route" }, 404);
      if (request.method === "GET" && parts[3] === "state") { const statistics = this.store.statistics(); return json({ ...statistics, epoch: this.epoch, jobs: { active: this.gate.active, pending: this.gate.waiters.length, cachedReaders: [...(this.packCache?.pins.values() ?? [])].reduce((a, b) => a + b, 0), memoryUsed: this.env.BYTE_BUDGET.used, memoryPeak: this.env.BYTE_BUDGET.peak }, packCache: { ...this.packCache?.metrics, ...statistics.packCache } }); }
      if (request.method === "POST" && parts[3] === "initialize") { this.store.initialize(testFormat(request)); return json({ ready: true }); }
      if (request.method === "GET" && parts[3] === "refs") return json({ refs: this.store.exec("SELECT name,oid FROM refs ORDER BY name").map((row) => ({ name: Array.from(bytes(row.name)), oid: row.oid })) });
      if (request.method === "POST" && parts[3] === "intake") {
        const format = testFormat(request, this.env.REPO_FORMAT ?? 0);
        const limits = { capabilities: 255 };
        for (const [name, field] of [["canonical", "maxCanonicalBytes"], ["work", "maxTotalWorkBytes"], ["temporary", "maxTemporaryBytes"]]) {
          const value = request.headers.get(`x-test-${name}-bytes`);
          if (value !== null) limits[field] = BigInt(value) < BigInt(this.env.CODEC_LIMITS[field]) ? BigInt(value) : this.env.CODEC_LIMITS[field];
        }
        await owner.reserve?.();
        const codec = new Codec(this.module, format, { ...this.env.CODEC_LIMITS, ...limits });
        const stage = this.store.stage(format, true); owner.stages.add(stage);
        const result = await new Intake(this.store, codec, stage).run(request);
        return json({ ...result, epoch: this.epoch, usage: this.store.statistics().usage });
      }
      if (request.method === "POST" && parts[3] === "stage" && UUID.test(parts[4] ?? "") && parts[5] === "expire") {
        const stage = this.store.exec("SELECT id FROM stages WHERE id=? AND phase='verified' AND retained=1", parts[4])[0];
        if (!stage) return json({ error: "retained quarantine not found" }, 404);
        this.store.exec("UPDATE stages SET phase='failed',retained=0 WHERE id=?", stage.id);
        await this.ctx.storage.setAlarm(Date.now() + 60000);
        return json({ cleanupScheduled: true });
      }
      if (request.method === "GET" && parts[3] === "stage" && UUID.test(parts[4] ?? "")) {
        const stage = this.store.exec("SELECT * FROM stages WHERE id=?", parts[4])[0];
        if (!stage) return json({ error: "stage not found" }, 404);
        const after = Number(url.searchParams.get("after") ?? -1);
        if (!Number.isSafeInteger(after) || after < -1) return json({ error: "invalid cursor" }, 400);
        const objects = this.store.exec("SELECT node,oid,kind,size,depth,verified,scanned,wire_kind,base_stage,base_node,base_oid FROM stage_objects WHERE stage=? AND node>? ORDER BY node LIMIT 256", stage.id, after);
        return json({ stage, objects });
      }
      if (request.method === "GET" && parts[3] === "object" && UUID.test(parts[4] ?? "")) {
        const stage = this.store.exec("SELECT * FROM stages WHERE id=? AND phase IN ('verified','published')", parts[4])[0];
        const node = Number(url.searchParams.get("node"));
        if (!stage || !Number.isSafeInteger(node) || node < 12) return json({ error: "verified staged object not found" }, 404);
        const row = this.store.node(stage.id, node);
        if (!row?.verified || !row.scanned) return json({ error: "verified staged object not found" }, 404);
        await owner.reserve?.();
        const codec = new Codec(this.module, stage.format, this.env.CODEC_LIMITS);
        const source = new ObjectSource(this.store, codec, stage.id);
        try {
          if (row.wire_kind >= 5) await source.warm(source.base(row));
          const id = await source.process(row, false);
          const data = source.canonical.get(id);
          if (url.searchParams.get("data") === "1" && data) return new Response(data, { headers: { "Content-Type": "application/octet-stream", "X-Object-Id": id, "Cache-Control": "no-store" } });
          return json({ oid: id, kind: row.kind, size: row.size, epoch: this.epoch });
        } finally { await this.store.clearTemporary(stage.id); }
      }
      return json({ error: "unknown route" }, 404);
    } catch (error) {
      const response = json({ error: String(error.message ?? error), codecError: error.code ?? null }, error instanceof ApiError ? error.status : error instanceof RequestError || error instanceof CodecError ? 400 : 500);
      if (response.status === 503) response.headers.set("Retry-After", "1");
      return response;
    }
  }
}
