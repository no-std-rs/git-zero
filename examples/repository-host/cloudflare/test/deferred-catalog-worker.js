// Test-only binding inspection and interruption points. Rust owns every runner,
// assertion and Wrangler process; all SQL/R2 operations use the real bindings.
import worker, { RepositoryServices as PagesServices } from "./catalog-pages-worker.js";
import { decodePage } from "../src/catalog-pages.js";
export default worker;

export class RepositoryServices extends PagesServices {
  constructor(ctx, env) {
    const fault = { mode: "", fired: false, puts: 0, deletes: 0, trace: [], sql: [] };
    const record = (event) => { if (fault.trace.length < 4096) fault.trace.push(event); };
    const fire = (mode) => fault.mode === mode && !fault.fired && (fault.fired = true);
    const sql = new Proxy(ctx.storage.sql, { get(target, name) {
      if (name === "exec") return (statement, ...bindings) => {
        record(statement);
        if (fault.puts && /^(INSERT INTO|DELETE FROM) blob_catalog/.test(statement) && fire("catalog-after-put")) {
          throw new Error("test catalog interruption after actual R2 PUT");
        }
        const cursor = target.exec(statement, ...bindings);
        const unknown = statement.startsWith("INSERT INTO repository_state") && fire("cursor-unknown");
        return {
          toArray() {
            const rows = cursor.toArray();
            if (fault.sql.length < 4096) fault.sql.push({ statement,
              rowsRead: unknown ? null : cursor.rowsRead,
              rowsWritten: unknown ? null : cursor.rowsWritten });
            return rows;
          },
          get rowsRead() { return unknown ? undefined : cursor.rowsRead; },
          get rowsWritten() { return unknown ? undefined : cursor.rowsWritten; },
        };
      };
      const value = Reflect.get(target, name);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    const storage = new Proxy(ctx.storage, { get(target, name) {
      if (name === "sql") return sql;
      if (name === "sync") return async () => {
        record("sync:start");
        await target.sync();
        record("sync:durable");
        if ((!fault.puts && fire("barrier-sync")) || (fault.puts && fire("sync-after-put"))) {
          throw new Error("test lost actual durable sync reply");
        }
      };
      const value = Reflect.get(target, name);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    const context = new Proxy(ctx, { get(target, name) {
      if (name === "storage") return storage;
      const value = Reflect.get(target, name);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    const blobs = new Proxy(env.BLOBS, { get(target, name) {
      if (["put", "get", "head", "delete", "list"].includes(name)) return async (...args) => {
        record(`r2:${name}:${typeof args[0] === "string" ? args[0] : "inventory"}`);
        if (name === "put") fault.puts++;
        if (name === "delete") fault.deletes++;
        const value = await target[name](...args);
        if (name === "put" && fire("put-after")) throw new Error("test lost actual R2 PUT reply");
        return value;
      };
      const value = Reflect.get(target, name);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    super(context, { ...env, BLOBS: blobs });
    this.deferredFault = fault;
  }

  async handle(request) {
    const path = new URL(request.url).pathname;
    if (path === "/_services/test/deferred-fault" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      Object.assign(this.deferredFault, { mode: (await request.json()).mode,
        fired: false, puts: 0, deletes: 0, trace: [], sql: [] });
      return Response.json({ armed: this.deferredFault.mode });
    }
    if (path === "/_services/test/deferred" && request.method === "GET") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const trace = this.deferredFault.trace.slice(), sql = this.deferredFault.sql.slice();
      const catalog = this.exec("SELECT * FROM blob_catalog").map((row) => {
        if (row.entries == null) return row;
        let decoded = null, applied_generation = null, error = null;
        try { decoded = decodePage(row); applied_generation = decoded.appliedGeneration; }
        catch (failure) { error = String(failure.message); }
        return { ...row, entries: Array.from(new Uint8Array(row.entries)), decoded,
          applied_generation, error };
      });
      const row = this.exec("SELECT manifest FROM repository_state WHERE id = 1")[0];
      return Response.json({ catalog, high_water: this.exec("SELECT * FROM high_water"),
        manifest: row ? JSON.parse(row.manifest) : { generation: 0, refs: {}, packs: [] },
        peak: this.peakBytes, certain: this.catalogCertain,
        sqlite_bytes: this.ctx.storage.sql.databaseSize,
        debt: this.debt ?? null, debt_loaded: this.debtLoaded ?? false,
        trace, sql, fault: { mode: this.deferredFault.mode, fired: this.deferredFault.fired } });
    }
    if (path === "/_services/test/evict" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      await this.ctx.storage.sync();
      this.ctx.abort("test-only explicit Durable Object eviction");
    }
    if (path === "/_services/test/reload" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      this.reloadCatalog();
      return Response.json({ certain: this.catalogCertain });
    }
    if (path === "/_services/test/headers" && request.method === "GET") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      return Response.json(Object.fromEntries([...request.headers].filter(([name]) => name.startsWith("x-git-zero-"))));
    }
    const response = await super.handle(request);
    const fault = this.deferredFault;
    if (response.ok && !fault.fired && ["/_services/manifest/retire", "/_services/manifest/upload-retire"].includes(path)) {
      if (fault.mode === "malformed-retirement-reply") {
        fault.fired = true;
        return new Response("{", { headers: { "content-type": "application/json" } });
      }
      if (fault.mode === "lost-retirement-reply") {
        fault.fired = true;
        return Response.json({ error: "test lost completed retirement response" }, { status: 502 });
      }
      if (["wrong-outcome-order", "wrong-uploaded-member"].includes(fault.mode)) {
        fault.fired = true;
        const body = await response.json();
        if (fault.mode === "wrong-outcome-order") body.retirement.outcomes.reverse();
        else body.uploaded.len++;
        return Response.json(body);
      }
    }
    return response;
  }
}
