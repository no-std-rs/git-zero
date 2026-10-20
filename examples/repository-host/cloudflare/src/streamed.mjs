import module from "../build/git_zero_host_wasm.wasm";
import { Codec } from "./codec.mjs";
import { Intake } from "./intake.mjs";
import { ObjectSource } from "./source.mjs";
import { Store } from "./storage.mjs";
import { bytes } from "./storage.mjs";
import { publish } from "./publication.mjs";
import { FetchJob } from "./fetch.mjs";

const REPOSITORY = /^[A-Za-z0-9_-]{1,80}$/;
const UUID = /^[0-9a-f-]{36}$/;
const json = (body, status = 200) => Response.json(body, { status, headers: { "Cache-Control": "no-store" } });
const encode = (value) => new TextEncoder().encode(value);
const fromHex = (value) => Uint8Array.from(value.match(/../g), (pair) => Number.parseInt(pair, 16));
// Advertise only host-backed receive features. Push options are retained in
// durable staging metadata; this host has no application hooks that consume them.
const RECEIVE_CAPABILITIES = 255;

function authorized(request, env) {
  if (!env.HOST_TOKEN) return false;
  const header = request.headers.get("authorization") ?? "";
  if (header === `Bearer ${env.HOST_TOKEN}`) return true;
  if (!header.startsWith("Basic ")) return false;
  try { const decoded = atob(header.slice(6)); const split = decoded.indexOf(":"); return split >= 0 && decoded.slice(split + 1) === env.HOST_TOKEN; }
  catch { return false; }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/health") return json({ ready: true, runtime: "workerd", codec: "no_std Wasm", durableStorage: "R2 and SQLite", gitReceive: true, gitFetch: true });
    if (!authorized(request, env)) return new Response("authentication required", { status: 401, headers: { "WWW-Authenticate": 'Basic realm="git-zero"' } });
    const parts = url.pathname.split("/");
    const test = env.TEST_MODE === "true" && parts[1] === "_test";
    const namespace = test ? parts[2] : parts[1]?.endsWith(".git") ? parts[1].slice(0, -4) : null;
    if (!REPOSITORY.test(namespace ?? "") || (!test && parts.length > 4)) return json({ error: "unknown route" }, 404);
    const headers = new Headers(request.headers);
    headers.set("x-git-zero-namespace", namespace);
    const stub = env.REPOSITORIES.get(env.REPOSITORIES.idFromName(namespace));
    return stub.fetch(new Request(request, { headers }));
  },
};

export class OwnRepository {
  constructor(ctx, env) {
    this.ctx = ctx; this.env = env; this.store = null;
    this.queue = Promise.resolve(); this.streamDone = null; this.epoch = crypto.randomUUID();
  }
  fetch(request) {
    // A single write/read job owns the request-local codec/cache view. Storage
    // and ref publication remain authoritative across instance reconstruction.
    const job = this.queue.then(() => { this.streamDone = null; return this.handle(request); });
    this.queue = job.then(() => this.streamDone).catch(() => {});
    return job;
  }
  alarm() {
    const job = this.queue.then(async () => {
      const identity = this.ctx.storage.sql.exec("SELECT namespace FROM host_identity WHERE id=1").toArray()[0];
      if (!identity) return;
      this.store ??= new Store(this.ctx, this.env, identity.namespace);
      await this.store.recover();
    });
    this.queue = job.catch(() => {}); return job;
  }
  stream(generator, type) {
    let finish;
    this.streamDone = new Promise((resolve) => { finish = resolve; });
    return new Response(new ReadableStream({
      async pull(controller) {
        try {
          const next = await generator.next();
          if (next.done) { controller.close(); finish(); }
          else controller.enqueue(next.value);
        } catch (error) { controller.error(error); finish(); }
      },
      async cancel() { try { await generator.return(); } finally { finish(); } },
    }), { headers: { "Content-Type": type, "Cache-Control": "no-store" } });
  }
  async *advertise(codec) {
    yield codec.advertStart();
    let after = new Uint8Array();
    for (;;) {
      const refs = this.store.exec("SELECT name,oid FROM refs WHERE name>? ORDER BY name LIMIT 128", after.slice().buffer);
      if (!refs.length) break;
      for (const ref of refs) { after = bytes(ref.name); yield codec.advertRef(fromHex(ref.oid), after); }
    }
    yield codec.advertFinish();
  }
  async *report(codec, selected, outcomes, failure) {
    yield codec.statusStart(selected, failure ? encode("invalid pack or object graph") : new Uint8Array());
    for (const outcome of outcomes) yield codec.statusRef(bytes(outcome.name), outcome.error ? encode(outcome.error) : new Uint8Array());
    yield codec.statusFinish();
  }
  async receive(request, codec) {
    const stage = this.store.stage(codec.format);
    try {
      await new Intake(this.store, codec, stage).run(request);
    } catch (error) {
      const selected = error.selected ?? 0;
      if (!selected) return json({ error: "invalid receive request" }, 400);
      const names = error.commands ?? this.store.exec("SELECT name FROM stage_commands WHERE stage=? ORDER BY seq", stage);
      await this.store.abandon(stage);
      const outcomes = names.map((row) => ({ ...row, error: "unpacker error" }));
      return this.stream(this.report(codec, selected, outcomes, true), "application/x-git-receive-pack-result");
    }
    const outcomes = await publish(this.store, stage);
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
  async handle(request) {
    try {
      if (!authorized(request, this.env)) return json({ error: "unknown route" }, 404);
      const namespace = request.headers.get("x-git-zero-namespace");
      if (!REPOSITORY.test(namespace ?? "")) return json({ error: "invalid repository" }, 400);
      if (!this.store) { this.store = new Store(this.ctx, this.env, namespace); await this.store.recover(); }
      if (this.store.namespace !== namespace) throw new Error("repository identity differs");
      const url = new URL(request.url), parts = url.pathname.split("/");
      if (parts[1] !== "_test") {
        const format = this.store.exec("SELECT format FROM repository WHERE id=1")[0]?.format ?? Number(this.env.REPO_FORMAT ?? 0);
        this.store.initialize(format);
        const codec = new Codec(module, format, { capabilities: RECEIVE_CAPABILITIES });
        if (request.method === "GET" && parts[2] === "info" && parts[3] === "refs" && url.searchParams.get("service") === "git-receive-pack") return this.stream(this.advertise(codec), "application/x-git-receive-pack-advertisement");
        const version = codec.uploadVersion(request.headers.get("git-protocol"));
        if (request.method === "GET" && parts[2] === "info" && parts[3] === "refs" && url.searchParams.get("service") === "git-upload-pack") return this.stream(this.uploadAdvertisement(codec, version), "application/x-git-upload-pack-advertisement");
        if (request.method === "POST" && parts[2] === "git-receive-pack" && parts.length === 3 && request.headers.get("content-type")?.split(";")[0].trim() === "application/x-git-receive-pack-request") return await this.receive(request, codec);
        if (request.method === "POST" && parts[2] === "git-upload-pack" && parts.length === 3 && request.headers.get("content-type")?.split(";")[0].trim() === "application/x-git-upload-pack-request") {
          const fetch = new FetchJob(this.store, codec, version);
          try { await fetch.decode(request); }
          catch (error) { codec.uploadCancel(); fetch.reset(); throw error; }
          return this.stream(fetch.response(), "application/x-git-upload-pack-result");
        }
        return json({ error: "unknown Git route" }, 404);
      }
      if (this.env.TEST_MODE !== "true") return json({ error: "unknown route" }, 404);
      if (request.method === "GET" && parts[3] === "state") return json({ ...this.store.statistics(), epoch: this.epoch });
      if (request.method === "POST" && parts[3] === "initialize") { this.store.initialize(Number(request.headers.get("x-test-format") ?? 0)); return json({ ready: true }); }
      if (request.method === "GET" && parts[3] === "refs") return json({ refs: this.store.exec("SELECT name,oid FROM refs ORDER BY name").map((row) => ({ name: Array.from(bytes(row.name)), oid: row.oid })) });
      if (request.method === "POST" && parts[3] === "intake") {
        const format = Number(request.headers.get("x-test-format") ?? this.env.REPO_FORMAT ?? 0);
        const codec = new Codec(module, format, { capabilities: 255 });
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
        const codec = new Codec(module, stage.format);
        const source = new ObjectSource(this.store, codec, stage.id, namespace);
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
      return json({ error: String(error.message ?? error), codecError: error.code ?? null }, 400);
    }
  }
}
