// Actual-binding early representation gate. Rust owns assertions and runtimes.
import worker, { RepositoryServices as Services } from "./deferred-catalog-worker.js";
import { decodePage } from "../src/catalog-pages.js";
import { decodeInline, encodeInline } from "../src/inline-catalog.js";
export default worker;
export class RepositoryServices extends Services {
  constructor(ctx, env) {
    const fault = { mode: "", fired: false, puts: 0, writes: 0, trace: [] };
    const fire = (mode) => fault.mode === mode && !fault.fired && (fault.fired = true);
    const sql = new Proxy(ctx.storage.sql, { get(target, name) {
      if (name === "exec") return (statement, ...bindings) => {
        const coupled = /^(INSERT INTO|UPDATE) repository_state/.test(statement) && statement.includes("catalog_root");
        if (fault.puts && coupled && fire("fused-sql-before")) throw new Error("test before coupled cursor after actual PUT");
        const cursor = target.exec(statement, ...bindings);
        if (coupled) fault.writes++;
        return { toArray() {
          const rows = cursor.toArray();
          if (fault.puts && coupled && fire("fused-sql-after")) throw new Error("test after actual coupled cursor");
          return rows;
        }, get rowsRead() { return cursor.rowsRead; }, get rowsWritten() { return cursor.rowsWritten; } };
      };
      const value = Reflect.get(target, name); return typeof value === "function" ? value.bind(target) : value;
    } });
    const storage = new Proxy(ctx.storage, { get(target, name) {
      if (name === "sql") return sql;
      if (name === "transactionSync") return (callback) => target.transactionSync(() => {
        const result = callback();
        if (fault.puts && fault.writes && fire("fused-transaction")) throw new Error("test actual coupled transaction rollback");
        return result;
      });
      if (name === "sync") return async () => {
        if (fault.puts && fault.writes && fire("fused-sync-before")) throw new Error("test before coupled sync");
        await target.sync();
        if (fault.puts && fault.writes && fire("fused-sync-after")) throw new Error("test lost durable coupled sync reply");
      };
      const value = Reflect.get(target, name); return typeof value === "function" ? value.bind(target) : value;
    } });
    const context = new Proxy(ctx, { get(target, name) {
      if (name === "storage") return storage;
      const value = Reflect.get(target, name); return typeof value === "function" ? value.bind(target) : value;
    } });
    const blobs = new Proxy(env.BLOBS, { get(target, name) {
      if (name === "put") return async (...args) => { fault.puts++; return target.put(...args); };
      if (name === "delete") return async (...args) => {
        if (fire("fused-delete-first")) return target.delete(Symbol("real invalid DELETE argument"));
        return target.delete(...args);
      };
      const value = Reflect.get(target, name); return typeof value === "function" ? value.bind(target) : value;
    } });
    super(context, { ...env, BLOBS: blobs });
    this.inlineFault = fault;
  }
  async handle(request) {
    const path = new URL(request.url).pathname;
    if (path === "/_services/test/deferred-fault" && request.method === "POST") {
      const body = await request.clone().json();
      Object.assign(this.inlineFault, { mode: body.mode, fired: false, puts: 0, writes: 0, trace: [] });
    }
    if (path === "/_services/test/inline-corrupt" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      const body = await request.json(), row = this.exec("SELECT catalog_root FROM repository_state WHERE id = 1")[0];
      const bytes = new Uint8Array(row.catalog_root), view = new DataView(bytes.buffer);
      if (body.kind === "future-ack") view.setBigUint64(14, BigInt(Number.MAX_SAFE_INTEGER), true);
      else if (body.kind === "unsafe-peak") view.setBigUint64(6, 9007199254740992n, true);
      else if (body.kind === "duplicate" && bytes[5] > 1) bytes.set(bytes.subarray(22, 58), 58);
      else if (body.kind === "bad-size") view.setUint32(54, 16 * 1024 * 1024 + 1, true);
      else if (body.kind === "valid-high-peak") view.setBigUint64(6, BigInt(body.peak), true);
      else if (body.kind === "bad-magic") bytes[0] = 0;
      else throw new Error("invalid corruption fixture");
      this.exec("UPDATE repository_state SET catalog_root = ? WHERE id = 1", bytes.buffer);
      await this.ctx.storage.sync(); this.reloadCatalog();
      return Response.json({ certain: this.catalogCertain });
    }
    if (path === "/_services/test/inline-normalized-future" && request.method === "POST") {
      this.currentPhase = "maintenance"; this.bump("do_requests");
      this.repoPrefix = `${request.headers.get("x-hosting-repo")}/`;
      await this.demoteInline();
      const state = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0];
      const row = this.exec("SELECT * FROM blob_catalog WHERE key = ?", "pages/")[0];
      if (!row || new Uint8Array(row.entries)[4] !== 2) throw new Error("fixture requires sole v2 normalized root");
      const bytes = new Uint8Array(row.entries);
      new DataView(bytes.buffer).setBigUint64(6, BigInt(state.generation + 1), true);
      this.exec("UPDATE blob_catalog SET entries = ? WHERE key = ?", bytes.buffer, "pages/");
      await this.ctx.storage.sync(); this.reloadCatalog();
      return Response.json({ generation: state.generation, certain: this.catalogCertain });
    }
    if (path === "/_services/test/inline-prototype" && request.method === "POST") {
      this.currentPhase = "push"; this.bump("do_requests");
      const body = await request.json(), prefix = `${request.headers.get("x-hosting-repo")}/`;
      if (!this.exec("PRAGMA table_info(repository_state)").some((column) => column.name === "catalog_root")) {
        this.exec("ALTER TABLE repository_state ADD COLUMN catalog_root BLOB");
      }
      let state = this.exec("SELECT * FROM repository_state WHERE id = 1")[0];
      if (body.promote) {
        const rows = this.exec("SELECT * FROM blob_catalog");
        if (rows.length !== 1 || rows[0].key !== "pages/") throw new Error("prototype requires sole root");
        const entries = decodePage(rows[0]);
        this.ctx.storage.transactionSync(() => {
          this.exec("UPDATE repository_state SET catalog_root = ? WHERE id = 1", encodeInline(entries, this.peakBytes, state.generation));
          this.exec("DELETE FROM blob_catalog WHERE key = ?", "pages/");
        });
        await this.ctx.storage.sync();
        return Response.json({ promoted: true, bytes: this.ctx.storage.sql.databaseSize });
      }
      const root = decodeInline(state.catalog_root, state.generation), entries = root.entries;
      const before = { ...this.totals };
      if (body.gc) {
        const objects = await this.inventory(prefix);
        const peak = Math.max(root.peak, objects.reduce((sum, object) => sum + object.size, 0));
        const live = new Set(body.live), retained = [];
        // Retain a durable full pre-delete witness before physical removals.
        await this.checkpointPeak(peak);
        for (const object of objects) {
          const key = object.key.slice(prefix.length);
          if (live.has(key)) retained.push({ digest: key.slice(7), size: object.size });
          else await this.operation("r2_delete", () => this.env.BLOBS.delete(object.key), () => 0, 0, key);
        }
        retained.sort((left, right) => left.digest < right.digest ? -1 : 1);
        this.exec("UPDATE repository_state SET catalog_root = ? WHERE id = 1", encodeInline(retained, peak, state.generation));
        await this.ctx.storage.sync();
        return Response.json({ peak, bytes: this.ctx.storage.sql.databaseSize, objects: retained.length });
      }
      if (body.chunk) {
        const bytes = new Uint8Array(body.bytes);
        await this.operation("r2_put", () => this.env.BLOBS.put(prefix + body.chunk, bytes), () => 0, bytes.length, body.chunk);
        const old = entries.find((entry) => entry.digest === body.chunk.slice(7));
        if (old) old.size = bytes.length; else entries.push({ digest: body.chunk.slice(7), size: bytes.length });
        entries.sort((left, right) => left.digest < right.digest ? -1 : 1);
      }
      const peak = Math.max(root.peak, entries.reduce((sum, entry) => sum + entry.size, 0));
      const published = this.ctx.storage.transactionSync(() => {
        state = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0];
        const won = state.generation === body.expected;
        const next = won ? body.next : JSON.parse(this.exec("SELECT manifest FROM repository_state WHERE id = 1")[0].manifest);
        this.exec("UPDATE repository_state SET generation = ?, manifest = ?, catalog_root = ? WHERE id = 1", next.generation, JSON.stringify(next), encodeInline(entries, peak, root.appliedGeneration));
        return won;
      });
      await this.ctx.storage.sync();
      const delta = Object.fromEntries(Object.keys(before).map((key) => [key, this.totals[key] - before[key]]));
      return Response.json({ published, delta, peak, bytes: this.ctx.storage.sql.databaseSize, root_bytes: 22 + entries.length * 36 });
    }
    if (path === "/_services/test/inline-prototype-state" && request.method === "GET") {
      this.currentPhase = "stats"; this.bump("do_requests");
      const state = this.exec("SELECT * FROM repository_state WHERE id = 1")[0];
      const root = decodeInline(state.catalog_root, state.generation);
      const objects = await this.inventory(`${request.headers.get("x-hosting-repo")}/`);
      return Response.json({ bytes: this.ctx.storage.sql.databaseSize, root, manifest: JSON.parse(state.manifest), objects: objects.length });
    }
    if (path === "/_services/test/inline-state" && request.method === "GET") {
      this.currentPhase = "stats"; this.bump("do_requests");
      const trace = this.deferredFault.trace.slice(), sql = this.deferredFault.sql.slice();
      const state = this.exec("SELECT * FROM repository_state WHERE id = 1")[0];
      let root = null, error = null;
      if (state?.catalog_root != null) { try { root = decodeInline(state.catalog_root, state.generation); } catch (failure) { error = String(failure.message); } }
      return Response.json({ root, root_error:error, catalog:this.exec("SELECT * FROM blob_catalog"),
        manifest:state?JSON.parse(state.manifest):{generation:0,refs:{},packs:[]},
        high_water:this.exec("SELECT * FROM high_water"), peak:this.peakBytes, live_bytes:this.liveBytes,
        live_objects:this.liveObjects, certain:this.catalogCertain, debt:this.debt,
        sqlite_bytes:this.ctx.storage.sql.databaseSize, trace, sql });
    }
    return super.handle(request);
  }
}
