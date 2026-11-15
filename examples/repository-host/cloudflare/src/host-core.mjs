import { Codec, CodecError } from "./codec.mjs";
import { Intake } from "./intake.mjs";
import { ObjectSource } from "./source.mjs";
import { Store } from "./storage.mjs";
import { bytes } from "./storage.mjs";
import { publish } from "./publication.mjs";
import { FetchJob } from "./fetch.mjs";
import { streamResponse } from "./response.mjs";
import { RequestError, isStorageFailure } from "./errors.mjs";

import { Lifecycle } from "./lifecycle.mjs";
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
  constructor(ctx, env, module, gate, maxRequests) {
    this.ctx = ctx; this.env = env; this.module = module; this.gate = gate; this.maxRequests = maxRequests; this.pendingRequests = 0; this.store = null;
    this.queue = Promise.resolve(); this.streamDone = null; this.epoch = crypto.randomUUID();
  }
  async fetch(request) {
    // Capture authorization at arrival, independently of scratch ownership,
    // recovery, alarm jobs and the lifetime of an earlier response stream.
    const namespace = request.headers.get("x-git-zero-namespace");
    const access = await authorize(request, this.env, namespace);
    if (access instanceof Response) return this.env.AUTH_MODE === "gateway" ? access : json({ error: "unknown route" }, 404);
    const ticket = this.pendingRequests < this.maxRequests ? this.gate.reserve() : null;
    if (!ticket) return new Response("repository host busy; retry later", { status: 503, headers: { "Retry-After": "1", "Cache-Control": "no-store" } });
    this.pendingRequests++;
    // A single write/read job owns the request-local codec/cache view. Storage
    // and ref publication remain authoritative across instance reconstruction.
    const job = this.queue.then(async () => {
      const release = await ticket.acquire(); this.streamDone = null;
      try { return await this.handle(request, access); }
      finally { Promise.resolve(this.streamDone).catch(() => {}).finally(() => { this.pendingRequests--; release(); }); }
    });
    this.queue = job.then(() => this.streamDone).catch(() => {});
    return job;
  }
  alarm() {
    const ticket = this.gate.reserve();
    if (!ticket) return this.ctx.storage.setAlarm(Date.now() + 60000);
    const job = this.queue.then(async () => {
      const release = await ticket.acquire();
      try {
      if (await this.ctx.storage.get("host.deletion.v1")) return;
      if (!this.ctx.storage.sql.exec("SELECT 1 FROM sqlite_master WHERE type='table' AND name='host_identity'").toArray().length) return;
      const identity = this.ctx.storage.sql.exec("SELECT namespace FROM host_identity WHERE id=1").toArray()[0];
      if (!identity) return;
      this.store ??= new Store(this.ctx, this.env, identity.namespace);
      await this.store.recover();
      } finally { release(); }
    });
    this.queue = job.catch(() => {}); return job;
  }
  stream(generator, type, length) {
    const result = streamResponse(generator, type, 60000, length);
    this.streamDone = result.done;
    return result.response;
  }
  async *advertise(codec) {
    yield codec.advertStart();
    let after = new Uint8Array();
    for (;;) {
      const refs = this.store.exec("SELECT name,oid FROM refs WHERE name>? ORDER BY name LIMIT ?", after.slice().buffer, this.env.MEMORY.refPageRows);
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
  async receive(request, codec, actor) {
    const stage = this.store.stage(codec.format);
    try {
      await new Intake(this.store, codec, stage).run(request);
    } catch (error) {
      if (isStorageFailure(error)) throw error;
      const selected = error.selected ?? 0;
      if (!selected) return json({ error: "invalid receive request" }, 400);
      const names = error.commands ?? this.store.exec("SELECT name FROM stage_commands WHERE stage=? ORDER BY seq", stage);
      // Intake has already reclaimed the stage or scheduled its recovery.
      // Report the original validation failure without retrying cleanup here.
      const reason = error instanceof PolicyError ? error.message : null;
      const outcomes = names.map((row) => ({ ...row, error: reason ?? "unpacker error" }));
      return this.stream(this.report(codec, selected, outcomes, reason ?? true), "application/x-git-receive-pack-result");
    }
    const outcomes = await publish(this.store, stage, codec, actor);
    const publication = this.store.exec("SELECT selected,phase FROM stages WHERE id=?", stage)[0];
    if (publication.phase !== "published") {
      try { await this.store.abandon(stage); }
      catch (error) {
        // Ref outcomes are already durable. Abandon keeps the failed stage's
        // ownership records and schedules recovery; cleanup cannot undo them.
        console.error(JSON.stringify({ event: "cleanup-deferred", stage, error: String(error.message ?? error) }));
      }
    }
    return this.stream(this.report(codec, publication.selected, outcomes, false), "application/x-git-receive-pack-result");
  }
  async *uploadAdvertisement(codec, version) {
    yield codec.uploadAdvert(version, true);
    if (version !== 2) yield* new FetchJob(this.store, codec, version).references();
  }
  async handle(request, access) {
    try {
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
      if (this.ctx.storage.sql.exec("SELECT 1 FROM sqlite_master WHERE type='table' AND name='host_storage_config'").toArray().length) {
        const stored = this.ctx.storage.sql.exec("SELECT prefix FROM host_storage_config WHERE id=1").toArray()[0];
        if (stored && stored.prefix !== this.env.KEY_PREFIX) throw new ApiError("stored R2 key prefix differs from configuration", 409);
      } else if (this.env.KEY_PREFIX && this.ctx.storage.sql.exec("SELECT 1 FROM sqlite_master WHERE type='table' AND name='host_identity'").toArray().length) {
        throw new ApiError("legacy repository requires the original empty R2 key prefix", 409);
      }
      this.lifecycle ??= new Lifecycle(this.ctx, this.env, namespace);
      await this.lifecycle.load();
      if (this.lifecycle.state) {
        const response = await this.lifecycle.retired(request);
        if (this.lifecycle.state.state === "deleted") this.store = null;
        return response;
      }
      if (!this.store) {
        const store = new Store(this.ctx, this.env, namespace);
        await store.recover(); this.store = store;
      }
      if (this.store.namespace !== namespace) throw new Error("repository identity differs");
      for (const field of ["r2FenceGets", "r2Failures", "r2Retries"]) { this.store.usage[field] += this.lifecycle.usage[field]; this.lifecycle.usage[field] = 0; }
      const url = new URL(request.url), parts = url.pathname.split("/");
      if (apiRoute(request) === "repository") {
        const response = await this.lifecycle.handle(request, this.store);
        if (this.lifecycle.state?.state === "deleted") this.store = null;
        return response;
      }
      if (apiRoute(request) === "policy") return await policyRoute(this.store, request);
      if (READ_ROUTES.has(apiRoute(request))) {
        const state = this.store.exec("SELECT format FROM repository WHERE id=1")[0];
        if (!state) return json({ error: "unknown route" }, 404);
        if (apiRoute(request) === "changes") return changes(this.store, request);
        return await new ReadApi(this.store, this.module, state.format, this.env.READ_LIMITS).handle(apiRoute(request), request, (generator, type, length) => this.stream(generator, type, length));
      }
      if (apiRoute(request)) return json({ error: "unknown route" }, 404);
      if (parts[1] !== "_test") {
        const format = this.store.exec("SELECT format FROM repository WHERE id=1")[0]?.format ?? Number(this.env.REPO_FORMAT ?? 0);
        if ((this.env.AUTH_MODE === "gateway" || this.env.IMPLICIT_CREATE === "false") && !this.store.exec("SELECT id FROM repository WHERE id=1").length) return json({ error: "unknown route" }, 404);
        this.store.initialize(format);
        const codec = new Codec(this.module, format, { ...this.env.CODEC_LIMITS, capabilities: RECEIVE_CAPABILITIES });
        if (request.method === "GET" && parts[2] === "info" && parts[3] === "refs" && url.searchParams.get("service") === "git-receive-pack") return this.stream(this.advertise(codec), "application/x-git-receive-pack-advertisement");
        const version = codec.uploadVersion(request.headers.get("git-protocol"));
        if (request.method === "GET" && parts[2] === "info" && parts[3] === "refs" && url.searchParams.get("service") === "git-upload-pack") return this.stream(this.uploadAdvertisement(codec, version), "application/x-git-upload-pack-advertisement");
        const contentType = request.headers.get("content-type")?.split(";")[0].trim().toLowerCase();
        if (request.method === "POST" && parts[2] === "git-receive-pack" && parts.length === 3 && contentType === "application/x-git-receive-pack-request") return await this.receive(request, codec, access.actor);
        if (request.method === "POST" && parts[2] === "git-upload-pack" && parts.length === 3 && contentType === "application/x-git-upload-pack-request") {
          const fetch = new FetchJob(this.store, codec, version);
          try { await fetch.decode(request); }
          catch (error) { codec.uploadCancel(); fetch.reset(); throw error; }
          return this.stream(fetch.response(), "application/x-git-upload-pack-result");
        }
        return json({ error: "unknown Git route" }, 404);
      }
      if (this.env.TEST_MODE !== "true") return json({ error: "unknown route" }, 404);
      if (request.method === "GET" && parts[3] === "state") return json({ ...this.store.statistics(), epoch: this.epoch });
      if (request.method === "POST" && parts[3] === "initialize") { this.store.initialize(testFormat(request)); return json({ ready: true }); }
      if (request.method === "GET" && parts[3] === "refs") return json({ refs: this.store.exec("SELECT name,oid FROM refs ORDER BY name").map((row) => ({ name: Array.from(bytes(row.name)), oid: row.oid })) });
      if (request.method === "POST" && parts[3] === "intake") {
        const format = testFormat(request, this.env.REPO_FORMAT ?? 0);
        const limits = { capabilities: 255 };
        for (const [name, field] of [["canonical", "maxCanonicalBytes"], ["work", "maxTotalWorkBytes"], ["temporary", "maxTemporaryBytes"]]) {
          const value = request.headers.get(`x-test-${name}-bytes`);
          if (value !== null) limits[field] = BigInt(value) < BigInt(this.env.CODEC_LIMITS[field]) ? BigInt(value) : this.env.CODEC_LIMITS[field];
        }
        const codec = new Codec(this.module, format, { ...this.env.CODEC_LIMITS, ...limits });
        const stage = this.store.stage(format, true);
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
      return json({ error: String(error.message ?? error), codecError: error.code ?? null }, error instanceof ApiError ? error.status : error instanceof RequestError || error instanceof CodecError ? 400 : 500);
    }
  }
}
