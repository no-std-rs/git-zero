import module from "../build/git_zero_host_wasm.wasm";
import { Codec } from "./codec.mjs";
import { Intake } from "./intake.mjs";
import { ObjectSource } from "./source.mjs";
import { Store } from "./storage.mjs";

const REPOSITORY = /^[A-Za-z0-9_-]{1,80}$/;
const UUID = /^[0-9a-f-]{36}$/;
const json = (body, status = 200) => Response.json(body, { status, headers: { "Cache-Control": "no-store" } });

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
    if (url.pathname === "/health") return json({ ready: true, runtime: "workerd", codec: "no_std Wasm", durableStorage: "R2 and SQLite", gitPublication: false });
    if (!authorized(request, env)) return new Response("authentication required", { status: 401, headers: { "WWW-Authenticate": 'Basic realm="git-zero"' } });
    const parts = url.pathname.split("/");
    if (env.TEST_MODE !== "true" || parts[1] !== "_test" || !REPOSITORY.test(parts[2] ?? "")) return json({ error: "unknown route" }, 404);
    const headers = new Headers(request.headers);
    headers.set("x-git-zero-namespace", parts[2]);
    const stub = env.REPOSITORIES.get(env.REPOSITORIES.idFromName(parts[2]));
    return stub.fetch(new Request(request, { headers }));
  },
};

export class OwnRepository {
  constructor(ctx, env) {
    this.ctx = ctx; this.env = env; this.store = null;
    this.queue = Promise.resolve(); this.epoch = crypto.randomUUID();
  }
  fetch(request) {
    // A single write/read job owns the request-local codec/cache view. Storage
    // and ref publication remain authoritative across instance reconstruction.
    const job = this.queue.then(() => this.handle(request));
    this.queue = job.catch(() => {});
    return job;
  }
  async handle(request) {
    try {
      if (this.env.TEST_MODE !== "true" || !authorized(request, this.env)) return json({ error: "unknown route" }, 404);
      const namespace = request.headers.get("x-git-zero-namespace");
      if (!REPOSITORY.test(namespace ?? "")) return json({ error: "invalid repository" }, 400);
      this.store ??= new Store(this.ctx, this.env, namespace);
      if (this.store.namespace !== namespace) throw new Error("repository identity differs");
      const url = new URL(request.url), parts = url.pathname.split("/");
      if (request.method === "GET" && parts[3] === "state") return json({ ...this.store.statistics(), epoch: this.epoch });
      if (request.method === "POST" && parts[3] === "intake") {
        const format = Number(request.headers.get("x-test-format") ?? this.env.REPO_FORMAT ?? 0);
        const codec = new Codec(module, format, { capabilities: 255 });
        const stage = this.store.stage(format);
        const result = await new Intake(this.store, codec, stage).run(request);
        return json({ ...result, epoch: this.epoch, usage: this.store.statistics().usage });
      }
      if (request.method === "GET" && parts[3] === "stage" && UUID.test(parts[4] ?? "")) {
        const stage = this.store.exec("SELECT * FROM stages WHERE id=?", parts[4])[0];
        if (!stage) return json({ error: "stage not found" }, 404);
        const after = Number(url.searchParams.get("after") ?? -1);
        if (!Number.isSafeInteger(after) || after < -1) return json({ error: "invalid cursor" }, 400);
        const objects = this.store.exec("SELECT node,oid,kind,size,depth,verified,scanned FROM stage_objects WHERE stage=? AND node>? ORDER BY node LIMIT 256", stage.id, after);
        return json({ stage, objects });
      }
      if (request.method === "GET" && parts[3] === "object" && UUID.test(parts[4] ?? "")) {
        const stage = this.store.exec("SELECT * FROM stages WHERE id=? AND phase='verified'", parts[4])[0];
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
