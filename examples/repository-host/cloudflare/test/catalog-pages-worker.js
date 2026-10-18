// Test-only inspection/fault wrapper. Every execution driver is Rust.
import worker, { RepositoryServices as PeakServices } from "./peak-catalog-worker.js";
import { ancestors, decodePage } from "../src/catalog-pages.js";
export default worker;

export class RepositoryServices extends PeakServices {
  constructor(ctx, env) {
    const fault = { mode: "", fired: false, deletes: 0, trace: [] };
    const record = (event) => { if (fault.trace.length < 512) fault.trace.push(event); };
    const fire = (mode) => fault.mode === mode && !fault.fired && (fault.fired = true);
    const sql = new Proxy(ctx.storage.sql, {
      get(target, name) {
        if (name === "exec") return (statement, ...bindings) => {
          if (statement.startsWith("INSERT INTO high_water")) record("fallback:write");
          if (statement === "ALTER TABLE blob_catalog ADD COLUMN entries BLOB" && fire("entries-before")) throw new Error("test interruption before entries schema cursor");
          if (fault.deletes && /^(INSERT INTO|DELETE FROM) blob_catalog/.test(statement) && fire("retire-catalog")) throw new Error("test catalog failure after physical deletes");
          const cursor = target.exec(statement, ...bindings);
          return { toArray() {
            const rows = cursor.toArray();
            if (statement === "ALTER TABLE blob_catalog ADD COLUMN entries BLOB" && fire("entries-after")) throw new Error("test lost entries schema cursor reply");
            if (statement.startsWith("INSERT INTO blob_catalog") && fire("catalog-after")) throw new Error("test lost actual catalog cursor reply");
            return rows;
          }, get rowsRead() { return cursor.rowsRead; }, get rowsWritten() { return cursor.rowsWritten; } };
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const storage = new Proxy(ctx.storage, {
      get(target, name) {
        if (name === "sql") return sql;
        if (name === "transactionSync") return (callback) => target.transactionSync(() => {
          const result = callback();
          if (fire("transaction") || (fault.deletes && fire("retire-transaction"))) throw new Error("test transaction rollback after actual cursors");
          return result;
        });
        if (name === "sync") return async () => {
          record("sync:start");
          await target.sync();
          record("sync:durable");
          if (fault.deletes && fire("retire-sync")) throw new Error("test ambiguous final retirement sync");
        };
        const value = Reflect.get(target, name);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const context = new Proxy(ctx, { get(target, name) {
      if (name === "storage") return storage;
      const value = Reflect.get(target, name);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    const blobs = new Proxy(env.BLOBS, { get(target, name) {
      if (name === "delete") return (...args) => {
        record("r2:delete");
        fault.deletes++;
        if (fault.deletes === 2 && fire("delete-second")) return target.delete(Symbol("real invalid DELETE argument"));
        return target.delete(...args);
      };
      const value = Reflect.get(target, name);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    super(context, { ...env, BLOBS: blobs });
    this.pageFault = fault;
  }

  async handle(request) {
    const path = new URL(request.url).pathname;
    if (path === "/_services/test/corrupt-number" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const body = await request.json();
      const literal = { negative: "-1", unsafe: "9007199254740992", fractional: "0.5", maxint: "9223372036854775807" }[body.kind];
      if (!literal || !["size", "peak_bytes", "fallback"].includes(body.field)) throw new Error("invalid test numeric corruption");
      if (body.field === "fallback") this.exec(`INSERT INTO high_water (id, bytes) VALUES (1, ${literal}) ON CONFLICT(id) DO UPDATE SET bytes = excluded.bytes`);
      else if (body.all) this.exec(`UPDATE blob_catalog SET ${body.field} = ${literal}`);
      else this.exec(`UPDATE blob_catalog SET ${body.field} = ${literal} WHERE key = ?`, body.key);
      // Leave rediscovery to the actual production reload/GC or next process.
      await this.ctx.storage.sync();
      return Response.json({ corrupted: true });
    }
    if (path === "/_services/test/probe-forms" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const key = (await request.json()).key, keys = [key, ...ancestors(key.slice(7))];
      const report = [];
      for (const form of ["in", "union-all-2", "union-all-4", "union-all-8", "union-all-16", "union-all-24", "union-all-32", "points"]) {
        const before = this.totals.do_rows_read;
        let rows;
        try {
        if (form === "in") rows = this.exec(`SELECT key FROM blob_catalog WHERE key IN (${keys.map(() => "?").join(",")})`, ...keys);
        else if (form.startsWith("union-all-")) {
          const maximum = Number(form.slice("union-all-".length));
          rows = [];
          for (let offset = 0; offset < keys.length; offset += maximum) {
            const batch = keys.slice(offset, offset + maximum);
            rows.push(...this.exec(batch.map(() => "SELECT key FROM blob_catalog WHERE key = ?").join(" UNION ALL "), ...batch));
          }
        }
        else rows = keys.flatMap((candidate) => this.exec("SELECT key FROM blob_catalog WHERE key = ?", candidate));
        report.push({ form, rowsRead: this.totals.do_rows_read - before, keys: rows.map((row) => row.key) });
        } catch (error) { report.push({ form, error: String(error.message) }); }
      }
      return Response.json(report);
    }
    if (path === "/_services/test/page-fault" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      this.pageFault.mode = (await request.json()).mode;
      this.pageFault.fired = false; this.pageFault.deletes = 0; this.pageFault.trace = [];
      return Response.json({ armed: this.pageFault.mode });
    }
    if (path === "/_services/test/seed" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const body = await request.json();
      this.ctx.storage.transactionSync(() => {
        if (body.reset) this.exec("DELETE FROM blob_catalog");
        for (const row of body.rows) this.exec("INSERT INTO blob_catalog (key, size, peak_bytes, entries) VALUES (?, ?, ?, ?)", row.key, row.size, row.peak_bytes, row.entries == null ? null : new Uint8Array(row.entries).buffer);
      });
      await this.ctx.storage.sync(); this.reloadCatalog();
      return Response.json({ seeded: true });
    }
    if (path === "/_services/test/metadata-put" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const body = await request.json();
      this.requireCatalogCertain();
      try {
        this.putCatalogPage(body.key, body.size, this.catalogLookup(body.key), body.peak ?? this.peakBytes);
        await this.ctx.storage.sync(); this.reloadCatalog();
      } catch (error) { this.catalogUncertain(); throw error; }
      return Response.json({ inserted: true });
    }
    if (path === "/_services/test/lookup" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const found = this.catalogLookup((await request.json()).key);
      return Response.json({ legacy: found.legacy?.key, page: found.page?.key, size: found.size });
    }
    if (path === "/_services/test/orphan" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const body = await request.json();
      const repo = request.headers.get("x-hosting-repo");
      const data = new Uint8Array(body.bytes);
      await this.operation("r2_put", () => this.env.BLOBS.put(`${repo}/${body.key}`, data), () => 0, data.byteLength, body.key);
      return Response.json({ created: true });
    }
    if (path === "/_services/test/pages" && request.method === "GET") {
      this.currentPhase = "maintenance";
      this.bump("do_requests");
      const schema = this.exec("PRAGMA table_info(blob_catalog)");
      const catalog = this.exec("SELECT * FROM blob_catalog").map((row) => {
        let decoded;
        if (row.entries != null) { try { decoded = decodePage(row); } catch { decoded = null; } }
        return { ...row, ...(row.entries != null ? { entries: Array.from(new Uint8Array(row.entries)), decoded } : {}) };
      });
      return Response.json({ schema, catalog, high_water: this.exec("SELECT * FROM high_water"),
        peak: this.peakBytes, certain: this.catalogCertain, owner: this.peakOwner,
        trace: this.testFault.trace.slice(), sql: this.testFault.sql.slice(), fired: this.testFault.fired, page_fault: this.pageFault });
    }
    return super.handle(request);
  }
}
