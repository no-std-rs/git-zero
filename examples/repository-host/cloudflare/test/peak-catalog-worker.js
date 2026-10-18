// Rust drives this test-only binding wrapper. Production has no fault routes.
import worker, { RepositoryServices as Services } from "../src/index.js";
export default worker;

export class RepositoryServices extends Services {
  constructor(ctx, env) {
    const fault = { mode: "", fired: false, trace: [], sql: [] };
    const record = (event) => { if (fault.trace.length < 512) fault.trace.push(event); };
    const fire = (mode) => fault.mode === mode && !fault.fired && (fault.fired = true);
    const sql = new Proxy(ctx.storage.sql, {
      get(target, name) {
        if (name === "exec") return (statement, ...bindings) => {
          record(statement);
          if ((statement.startsWith("INSERT INTO blob_catalog") && fire("catalog-put"))
              || (statement.startsWith("DELETE FROM blob_catalog WHERE") && fire("catalog-delete"))
              || (statement === "DELETE FROM blob_catalog" && fire("gc-reset"))
              || (statement.startsWith("INSERT INTO high_water") && fire("fallback-sql"))
              || (statement.startsWith("ALTER TABLE") && fire("migration"))) {
            throw new Error("test SQL interruption before actual cursor");
          }
          const cursor = target.exec(statement, ...bindings);
          return {
            toArray() {
              const rows = cursor.toArray();
              if (fault.sql.length < 512) fault.sql.push({ statement, rowsRead: cursor.rowsRead, rowsWritten: cursor.rowsWritten });
              if (statement.startsWith("ALTER TABLE") && fire("migration-after")) throw new Error("test interruption after actual schema migration");
              return rows;
            },
            get rowsRead() { return cursor.rowsRead; },
            get rowsWritten() { return cursor.rowsWritten; },
          };
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const storage = new Proxy(ctx.storage, {
      get(target, name) {
        if (name === "sql") return sql;
        if (name === "sync") return async () => {
          record("sync:start");
          await target.sync();
          record("sync:durable");
          if (fire("sync")) throw new Error("test ambiguous successful sync reply");
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const context = new Proxy(ctx, {
      get(target, name) {
        if (name === "storage") return storage;
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const blobs = new Proxy(env.BLOBS, {
      get(target, name) {
        if (["put", "delete", "list", "head"].includes(name)) return (...args) => {
          record(`r2:${name}`);
          if ((name === "put" && fire("put")) || (name === "delete" && fire("delete"))) {
            return target[name](Symbol("real invalid binding argument"), ...args.slice(1));
          }
          return target[name](...args);
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    super(context, { ...env, BLOBS: blobs });
    this.testFault = fault;
  }

  async handle(request) {
    const path = new URL(request.url).pathname;
    const fault = this.testFault;
    if (path.startsWith("/_services/test/")) {
      this.currentPhase = "maintenance";
      this.bump("do_requests");
      if (path === "/_services/test/fault" && request.method === "POST") {
        fault.mode = (await request.json()).mode;
        fault.fired = false;
        fault.trace = [];
        fault.sql = [];
        return Response.json({ armed: fault.mode });
      }
      if (path === "/_services/test/state" && request.method === "GET") {
        const schema = this.exec("PRAGMA table_info(blob_catalog)");
        return Response.json({ schema, catalog: this.exec("SELECT * FROM blob_catalog"),
          high_water: this.exec("SELECT * FROM high_water"),
          peak: this.peakBytes, certain: this.catalogCertain, owner: this.peakOwner,
          trace: fault.trace.slice(), sql: fault.sql.slice(), fired: fault.fired });
      }
    }
    const response = await super.handle(request);
    if (response.ok && fault.mode === "lost" && !fault.fired) {
      fault.fired = true;
      return Response.json({ error: "test lost completed response" }, { status: 502 });
    }
    return response;
  }
}
