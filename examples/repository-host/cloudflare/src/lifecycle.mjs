import { bucket } from "./r2-s3.mjs";
import { bytes, blob, retryStorage } from "./storage.mjs";
import { ApiError, apiRoute, readJson, json, hidden, encode } from "./api.mjs";

const STATE = "host.deletion.v1";
export const PURGE_PAGE = 64;
const text = (value) => new TextDecoder().decode(bytes(value));
export function branchRef(branch) {
  if (typeof branch !== "string" || encode(branch).length < 1 || encode(branch).length > 240 || (branch === "@" || branch === "HEAD") || /[\uD800-\uDFFF]/u.test(branch) || /[\x00-\x20\x7f~^:?*\[\\]/.test(branch) || branch.includes("..") || branch.includes("@{") || branch.endsWith(".") || branch.split("/").some((part) => !part || part.startsWith(".") || part.endsWith(".lock"))) throw new ApiError("invalid default branch (maximum 240 UTF-8 bytes)");
  return `refs/heads/${branch}`;
}
export async function creationSettings(request) {
  const value = await readJson(request);
  if (Object.keys(value).some((name) => !["objectFormat", "defaultBranch"].includes(name))) throw new ApiError("unknown create field");
  const format = value.objectFormat ?? "sha1", branch = branchRef(value.defaultBranch ?? "main");
  if (format !== "sha1" && format !== "sha256") throw new ApiError("objectFormat must be sha1 or sha256");
  return { format, branch };
}

// Content is removed through R2 listing and native storage.deleteAll(), rather
// than unbounded SQL DELETE loops. A separate permanent fence survives even a
// crash between deleteAll and re-saving the tiny Durable Object tombstone.
export class Lifecycle {
  constructor(ctx, env, namespace) {
    this.ctx = ctx; this.blobs = bucket(env); this.namespace = namespace;
    this.prefix = `${env.KEY_PREFIX ?? ""}repositories/${namespace}/`;
    this.fence = `${env.KEY_PREFIX ?? ""}repository-tombstones/v1/${namespace}`;
    this.loaded = false; this.state = null;
    this.usage = { r2FenceGets: 0, r2Failures: 0, r2Retries: 0 };
  }
  async load() {
    if (this.loaded) return;
    this.state = await this.ctx.storage.get(STATE) ?? null;
    const fence = this.state ? null : await retryStorage(async () => { this.usage.r2FenceGets++; return this.blobs.get(this.fence); }, this.usage);
    if (fence) {
      await fence.body?.cancel();
      this.state = { state: "deleting", namespace: this.namespace };
      await this.ctx.storage.put(STATE, this.state);
    }
    if (this.state && this.state.namespace !== this.namespace) throw new ApiError("unknown route", 404);
    this.loaded = true;
  }
  async retired(request) {
    if (apiRoute(request) !== "repository") return hidden();
    if (request.method === "DELETE") return this.state.state === "deleted" ? json({ state: "deleted", complete: true }) : this.purge();
    if (request.method === "GET") return json({ namespace: this.namespace, ...this.state, complete: this.state.state === "deleted" }, this.state.state === "deleted" ? 410 : 202);
    return json({ error: "namespace is permanently retired" }, 410);
  }
  describe(store) {
    const row = store.exec("SELECT * FROM repository WHERE id=1")[0];
    if (!row) return null;
    const settings = store.exec("SELECT * FROM host_settings WHERE id=1")[0];
    const accounting = store.storageAccounting();
    return {
      namespace: this.namespace, state: "active", objectFormat: row.format ? "sha256" : "sha1",
      head: { symbolic: text(row.head), oid: store.exec("SELECT oid FROM refs WHERE name=?", row.head)[0]?.oid ?? null },
      defaultBranch: settings?.default_branch ? text(settings.default_branch).slice(11) : null,
      generation: row.generation, objectCount: row.object_count,
      storedBytes: accounting.ready ? accounting.knownBytes : null, storageAccounting: accounting, databaseBytes: store.ctx.storage.sql.databaseSize ?? null, storedPackBytes: store.exec("SELECT COALESCE(SUM(bytes),0) AS size FROM stages WHERE phase='published'")[0].size,
      lastPublication: settings?.last_publication ?? null,
    };
  }
  async handle(request, store, creation) {
    const current = this.describe(store);
    if (request.method === "GET") return current ? json(current) : hidden();
    if (request.method === "PUT") {
      const { format, branch } = creation ?? await creationSettings(request);
      if (current) {
        if (current.objectFormat !== format || (current.defaultBranch ?? current.head.symbolic.slice(11)) !== branch.slice(11)) throw new ApiError("repository already exists with different creation settings", 409);
        return json(current);
      }
      store.ctx.storage.transactionSync(() => {
        store.initialize(format === "sha256" ? 1 : 0);
        store.exec("UPDATE repository SET head=? WHERE id=1", blob(encode(branch)));
        store.exec("INSERT INTO host_settings(id,default_branch,last_publication) VALUES(1,?,NULL)", blob(encode(branch)));
      });
      return json(this.describe(store), 201);
    }
    if (request.method === "DELETE") {
      if (!current) return hidden();
      return this.beginDelete();
    }
    return json({ error: "method not allowed" }, 405);
  }
  async beginDelete() {
    this.state = { state: "deleting", namespace: this.namespace, retiredAt: Date.now() };
    await this.ctx.storage.put(STATE, this.state);
    return this.purge();
  }
  async purge() {
    // Await the external fence before removing any content. Retrying a lost
    // acknowledgment rewrites the same fence; its presence always means retired.
    await this.blobs.put(this.fence, encode(JSON.stringify({ version: 1, ...this.state })));
    const page = await this.blobs.list({ prefix: this.prefix, limit: PURGE_PAGE });
    if (!Array.isArray(page.objects) || page.objects.length > PURGE_PAGE || page.objects.some((item) => typeof item.key !== "string" || !item.key.startsWith(this.prefix))) throw new Error("invalid repository purge listing");
    if (page.objects.length) {
      await this.blobs.delete(page.objects.map((item) => item.key));
      return json({ state: "deleting", complete: false, deletedObjects: page.objects.length }, 202);
    }
    if (page.truncated) throw new Error("empty truncated repository purge listing");
    await this.ctx.storage.deleteAlarm();
    await this.ctx.storage.deleteAll();
    this.state = { state: "deleted", namespace: this.namespace };
    await this.ctx.storage.put(STATE, this.state);
    return json({ state: "deleted", complete: true });
  }
}
