// Test-only binding/barrier wrapper, driven by Rust ignored contracts. No
// production fault switches, fake operation counters, or test runner live here.
import worker, { RepositoryServices as Services } from "../src/index.js";
export default worker;

export class RepositoryServices extends Services {
  constructor(ctx, env) {
    const fault = { mode: "", active: false, fired: false, syncs: 0, deletes: 0 };
    const sql = new Proxy(ctx.storage.sql, {
      get(target, name) {
        if (name === "exec") return (statement, ...bindings) => {
          if (fault.active && !fault.fired && fault.mode === "catalog" && statement.startsWith("INSERT INTO blob_catalog")) {
            fault.fired = true;
            throw new Error("test interruption after real PUT before catalog update");
          }
          return target.exec(statement, ...bindings);
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const storage = new Proxy(ctx.storage, {
      get(target, name) {
        if (name === "sql") return sql;
        if (name === "sync") return async () => {
          await target.sync();
          if (!fault.active) return;
          fault.syncs++;
          if (!fault.fired && ((fault.mode === "uploadsync" && fault.syncs === 1)
              || (fault.mode === "cassync" && fault.syncs === 2))) {
            fault.fired = true;
            throw new Error("test ambiguous durability barrier response");
          }
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
        if (name === "put") return (key, data) => {
          if (fault.active && !fault.fired && fault.mode === "put") {
            fault.fired = true;
            return target.put(Symbol("real invalid PUT argument"), data);
          }
          return target.put(key, data);
        };
        if (name === "delete") return (key) => {
          if (fault.active && !fault.fired && fault.mode === "delete" && ++fault.deletes === 2) {
            fault.fired = true;
            return target.delete(Symbol("real invalid DELETE argument"));
          }
          return target.delete(key);
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    super(context, { ...env, BLOBS: blobs });
    this.fault = fault;
  }

  async handle(request) {
    const fault = this.fault;
    fault.active = new URL(request.url).pathname === "/_services/manifest/upload-retire";
    fault.mode = /-upload-(put|catalog|uploadsync|cassync|delete|lost|corrupt)-/.exec(request.headers.get("x-hosting-repo"))?.[1] ?? "";
    fault.syncs = 0; fault.deletes = 0;
    try {
      const response = await super.handle(request);
      if (fault.active && !fault.fired && ["lost", "corrupt"].includes(fault.mode) && response.ok) {
        fault.fired = true;
        if (fault.mode === "lost") return Response.json({ error: "test lost committed reply" }, { status: 502 });
        const body = await response.json(); body.version = 2;
        return Response.json(body);
      }
      return response;
    } finally { fault.active = false; }
  }
}
