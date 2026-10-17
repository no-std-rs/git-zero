// Local workerd runtime glue. Git and all test/benchmark runners remain Rust.
// Never deploy this loopback codec gateway as a production Git service.
const registry = new Map();
const workerUsage = new Map();
let workerEpoch;
const PHASES = new Set(["startup", "push", "fetch", "maintenance", "stats"]);
const REPO = /^[A-Za-z0-9_-]{1,80}$/;
const BLOB = /^chunks\/[0-9a-f]{64}$/;
const MAX_BLOB_BYTES = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES = 1_500_000;
const MAX_BULK_KEYS = 256;
const MAX_BULK_CHUNK_BYTES = 8 * 1024 * 1024;
const MAX_BULK_RESPONSE_BYTES = 16 * 1024 * 1024;
const BULK_HEADER_BYTES = 12;
const BULK_LENGTH_BYTES = 8;
const USAGE_HEADER = "x-git-zero-service-usage";
const OPERATIONS = new Set(["r2_get", "r2_head", "r2_put", "r2_list", "r2_delete", "manifest_get", "manifest_cas"]);
const OP_FIELDS = ["attempts", "successes", "failures", "read_bytes", "written_bytes"];

function emptyUsage() {
  return {
    worker_requests: 0, worker_gateway_requests: 0, worker_service_requests: 0,
    worker_observer_requests: 0, do_requests: 0, r2_class_a: 0, r2_class_b: 0,
    r2_deletes: 0, r2_read_bytes: 0, r2_written_bytes: 0,
    do_rows_read: 0, do_rows_written: 0,
  };
}

function json(value, status = 200) {
  return Response.json(value, { status, headers: { "Cache-Control": "no-store" } });
}

function fail(status, message) {
  const error = new Error(message);
  error.status = status;
  throw error;
}

function phase(request, fallback = "unclassified") {
  const value = request.headers.get("x-hosting-phase");
  return PHASES.has(value) ? value : fallback;
}

async function bytes(request, maximum) {
  const declared = Number(request.headers.get("content-length"));
  if (declared > maximum) fail(413, "request body exceeds local service limit");
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  const parts = [];
  let length = 0;
  for (;;) {
    const item = await reader.read();
    if (item.done) break;
    length += item.value.byteLength;
    if (length > maximum) {
      await reader.cancel();
      fail(413, "request body exceeds local service limit");
    }
    parts.push(item.value);
  }
  const result = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) { result.set(part, offset); offset += part.byteLength; }
  return result;
}

async function parseJson(request, maximum) {
  try { return JSON.parse(new TextDecoder().decode(await bytes(request, maximum))); }
  catch (error) { if (error.status) throw error; fail(400, "invalid JSON"); }
}

function repoName(value) {
  if (typeof value !== "string" || !REPO.test(value)) fail(400, "invalid repository namespace");
  return value;
}

function workerCount(repo, kind, currentPhase) {
  if (!workerUsage.has(repo)) {
    if (workerUsage.size >= 512) fail(429, "local repository counter limit reached");
    workerUsage.set(repo, { totals: emptyUsage(), by_phase: {}, operations: {},
      accounting_complete: true, accounting_errors: [], do_instance_epoch: null });
  }
  const usage = workerUsage.get(repo);
  usage.by_phase[currentPhase] ??= emptyUsage();
  for (const counter of [usage.totals, usage.by_phase[currentPhase]]) {
    counter.worker_requests++;
    counter[`worker_${kind}_requests`]++;
  }
}

function incomplete(repo, reason) {
  const usage = workerUsage.get(repo);
  usage.accounting_complete = false;
  if (usage.accounting_errors.length < 8) usage.accounting_errors.push(reason);
}

function plainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validCount(value, nullable = false) {
  return (nullable && value === null) || (Number.isSafeInteger(value) && value >= 0);
}

function nullableCounter(key) { return key === "do_rows_read" || key === "do_rows_written"; }

function addCount(left, right) {
  if (left === null || right === null) return null;
  const result = left + right;
  if (!Number.isSafeInteger(result) || result < 0) throw new Error("service counter overflow");
  return result;
}

function addCounters(target, delta, fields = Object.keys(emptyUsage())) {
  for (const key of fields) target[key] = addCount(target[key] === undefined ? 0 : target[key], delta[key]);
}

function acceptReceipt(repo, encoded) {
  if (!encoded || encoded.length > 16384) throw new Error("missing or oversized service receipt");
  const receipt = JSON.parse(encoded);
  const fields = Object.keys(emptyUsage());
  if (receipt.version !== 1 || !plainObject(receipt.totals) || !plainObject(receipt.by_phase)
      || !plainObject(receipt.operations) || typeof receipt.do_instance_epoch !== "string"
      || !/^[0-9a-f-]{36}$/.test(receipt.do_instance_epoch)) throw new Error("invalid service receipt shape");
  const validateCounters = (counters) => {
    if (!plainObject(counters) || Object.keys(counters).length !== fields.length
        || fields.some((key) => !validCount(counters[key], nullableCounter(key)))
        || fields.some((key) => key.startsWith("worker_") && counters[key] !== 0)) {
      throw new Error("invalid service counter delta");
    }
  };
  validateCounters(receipt.totals);
  if (receipt.totals.do_requests !== 1 || Object.keys(receipt.by_phase).length > 6
      || Object.keys(receipt.operations).length > OPERATIONS.size) throw new Error("invalid service receipt cardinality");
  const phaseSum = emptyUsage();
  for (const [name, counters] of Object.entries(receipt.by_phase)) {
    if (!PHASES.has(name) && name !== "unclassified") throw new Error("invalid service receipt phase");
    validateCounters(counters);
    addCounters(phaseSum, counters);
  }
  if (fields.some((key) => receipt.totals[key] !== phaseSum[key])) throw new Error("inconsistent service phase totals");
  for (const [name, op] of Object.entries(receipt.operations)) {
    if (!OPERATIONS.has(name) || !plainObject(op) || Object.keys(op).length !== OP_FIELDS.length
        || OP_FIELDS.some((key) => !validCount(op[key]))
        || op.attempts !== op.successes + op.failures) throw new Error("invalid service operation delta");
  }
  const attempts = (name) => receipt.operations[name]?.attempts ?? 0;
  const sumBytes = (field) => Object.entries(receipt.operations)
    .filter(([name]) => name.startsWith("r2_")).reduce((sum, [, op]) => addCount(sum, op[field]), 0);
  if (receipt.totals.r2_class_a !== attempts("r2_put") + attempts("r2_list")
      || receipt.totals.r2_class_b !== attempts("r2_get") + attempts("r2_head")
      || receipt.totals.r2_deletes !== attempts("r2_delete")
      || receipt.totals.r2_read_bytes !== sumBytes("read_bytes")
      || receipt.totals.r2_written_bytes !== sumBytes("written_bytes")) throw new Error("inconsistent R2 operation totals");
  // Apply to a copy first: a malformed/overflowing receipt must never leave
  // half of its counters added before accounting is marked incomplete.
  const usage = workerUsage.get(repo);
  const next = structuredClone(usage);
  addCounters(next.totals, receipt.totals);
  for (const [name, counters] of Object.entries(receipt.by_phase)) {
    next.by_phase[name] ??= emptyUsage();
    addCounters(next.by_phase[name], counters);
  }
  for (const [name, counters] of Object.entries(receipt.operations)) {
    next.operations[name] ??= Object.fromEntries(OP_FIELDS.map((key) => [key, 0]));
    addCounters(next.operations[name], counters, OP_FIELDS);
  }
  next.do_instance_epoch = receipt.do_instance_epoch;
  workerUsage.set(repo, next);
}

function mergeWorkerUsage(result, repo) {
  const usage = workerUsage.get(repo);
  for (const key of Object.keys(usage.totals)) result[key] = usage.totals[key];
  result.by_phase = usage.by_phase;
  result.operations = usage.operations;
  result.counter_epoch = workerEpoch;
  result.do_instance_epoch = usage.do_instance_epoch;
  result.accounting_complete = usage.accounting_complete;
  result.accounting_errors = usage.accounting_errors;
  result.counter_scope = "current Worker runtime; actual per-RPC receipts survive Durable Object eviction; R2 inventory persists";
  return result;
}

function loopbackOrigin(value) {
  let origin;
  try { origin = new URL(value); } catch { fail(400, "invalid codec origin"); }
  if (origin.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname)
      || origin.username || origin.password || origin.pathname !== "/" || origin.search || origin.hash) {
    fail(400, "codec origin must be an HTTP loopback origin");
  }
  return origin.origin;
}

export default {
  async fetch(request, env) {
    try {
      workerEpoch ??= crypto.randomUUID();
      const url = new URL(request.url);
      const service = url.pathname.startsWith("/_services/");
      const parts = url.pathname.split("/");
      const prefixed = !service && parts[2] === "repo.git";
      const repo = repoName(service
        ? request.headers.get("x-hosting-repo") ?? env.REPO_ID ?? "demo"
        : prefixed ? parts[1] : env.REPO_ID ?? "demo");
      if (url.pathname === "/health") {
        workerCount(repo, "observer", "stats");
        return json({ ready: true, runtime: "workerd", local_only: true });
      }
      if (service) {
        const observer = url.pathname === "/_services/metrics";
        const currentPhase = observer ? "stats" : phase(request);
        workerCount(repo, observer ? "observer" : "service", currentPhase);
        if (!env.LOCAL_TOKEN || request.headers.get("x-hosting-token") !== env.LOCAL_TOKEN) {
          return json({ error: "invalid local service token" }, 401);
        }
        if (url.pathname === "/_services/register" && request.method === "POST") {
          const body = await parseJson(request, 1024);
          const origin = loopbackOrigin(body.codec_origin);
          if (registry.has(repo) && registry.get(repo) !== origin) {
            fail(409, "a different local codec is already registered; unregister it first");
          }
          if (!registry.has(repo) && registry.size >= 256) fail(429, "local codec registry limit reached");
          registry.set(repo, origin);
          return json({ registered: true, repository: repo, codec_origin: origin });
        }
        if (url.pathname === "/_services/unregister" && request.method === "POST") {
          registry.delete(repo);
          return json({ registered: false, repository: repo });
        }
        const headers = new Headers(request.headers);
        headers.set("x-hosting-repo", repo);
        headers.set("x-hosting-phase", currentPhase);
        // Bound and finish this small JSON upload before entering the DO gate.
        // Returning a DO error while its forwarded upload is still active can
        // leave workerd's request pump holding the repository gate indefinitely.
        const serviceRequest = url.pathname === "/_services/blobs/read" && request.method === "POST"
          ? new Request(request.url, { method: request.method, headers, body: await bytes(request, MAX_MANIFEST_BYTES) })
          : new Request(request, { headers });
        const stub = env.REPOSITORIES.get(env.REPOSITORIES.idFromName(repo));
        let response;
        try { response = await stub.fetch(serviceRequest); }
        catch (error) { incomplete(repo, "Durable Object dispatch failed; side effects may be unaccounted"); throw error; }
        try { acceptReceipt(repo, response.headers.get(USAGE_HEADER)); }
        catch { incomplete(repo, "missing or invalid Durable Object usage receipt"); }
        const cleanHeaders = new Headers(response.headers);
        cleanHeaders.delete(USAGE_HEADER);
        const clean = new Response(response.body, { status: response.status, statusText: response.statusText, headers: cleanHeaders });
        if (observer && clean.ok) return json(mergeWorkerUsage(await clean.json(), repo));
        return clean;
      }
      const forwardedPath = prefixed ? url.pathname.slice(repo.length + 1) : url.pathname;
      if (!forwardedPath.startsWith("/repo.git/")) return json({ error: "unknown route" }, 404);
      const origin = registry.get(repo) ?? env.CODEC_ORIGIN;
      if (!origin) return json({ error: "register a local Rust codec first" }, 503);
      const gitPhase = forwardedPath.endsWith("/git-receive-pack")
        || url.searchParams.get("service") === "git-receive-pack" ? "push" : "fetch";
      workerCount(repo, "gateway", gitPhase);
      const headers = new Headers(request.headers);
      headers.delete("host");
      headers.delete("x-hosting-token");
      headers.set("x-hosting-repo", repo);
      headers.set("x-hosting-phase", gitPhase);
      const target = loopbackOrigin(origin) + forwardedPath + url.search;
      // This awaits Rust outside a Durable Object. Rust can safely call back
      // into /_services while its Git request is in flight.
      return await fetch(new Request(target, {
        method: request.method, headers, body: request.body, redirect: "manual",
      }));
    } catch (error) {
      return json({ error: String(error.message ?? error) }, error.status ?? 500);
    }
  },
};

export class RepositoryServices {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.totals = emptyUsage();
    this.byPhase = {};
    this.operations = {};
    this.keyUsage = new Map();
    this.keyUsageTruncated = false;
    this.currentPhase = "startup";
    this.epoch = crypto.randomUUID();
    this.sqlRowsKnown = true;
    this.firstRequest = true;
    this.exec("CREATE TABLE IF NOT EXISTS repository_state (id INTEGER PRIMARY KEY, generation INTEGER NOT NULL, manifest TEXT NOT NULL)");
    this.exec("CREATE TABLE IF NOT EXISTS blob_catalog (key TEXT PRIMARY KEY, size INTEGER NOT NULL) WITHOUT ROWID");
    this.exec("CREATE TABLE IF NOT EXISTS high_water (id INTEGER PRIMARY KEY, bytes INTEGER NOT NULL)");
    const inventory = this.exec("SELECT COUNT(*) AS objects, COALESCE(SUM(size), 0) AS bytes FROM blob_catalog")[0];
    this.liveObjects = inventory.objects;
    this.liveBytes = inventory.bytes;
    this.peakBytes = Math.max(this.liveBytes, this.exec("SELECT bytes FROM high_water WHERE id = 1")[0]?.bytes ?? 0);
  }

  bump(name, value = 1) {
    this.totals[name] += value;
    this.byPhase[this.currentPhase] ??= emptyUsage();
    this.byPhase[this.currentPhase][name] += value;
  }

  exec(statement, ...bindings) {
    const cursor = this.ctx.storage.sql.exec(statement, ...bindings);
    const result = cursor.toArray();
    if (typeof cursor.rowsRead === "number" && typeof cursor.rowsWritten === "number") {
      this.bump("do_rows_read", cursor.rowsRead);
      this.bump("do_rows_written", cursor.rowsWritten);
    } else { this.sqlRowsKnown = false; }
    return result;
  }

  async operation(name, callback, readBytes = () => 0, writeBytes = 0, key) {
    const op = this.operations[name] ??= { attempts: 0, successes: 0, failures: 0, read_bytes: 0, written_bytes: 0 };
    let keyOp;
    if (key !== undefined) {
      keyOp = this.keyUsage.get(key);
      if (keyOp) this.keyUsage.delete(key);
      else {
        if (this.keyUsage.size >= 256) {
          this.keyUsage.delete(this.keyUsage.keys().next().value);
          this.keyUsageTruncated = true;
        }
        keyOp = { key, get_attempts: 0, head_attempts: 0, put_attempts: 0,
          delete_attempts: 0, failures: 0, read_bytes: 0, written_bytes: 0 };
      }
      keyOp[`${name.slice(3)}_attempts`]++;
      this.keyUsage.set(key, keyOp);
    }
    op.attempts++;
    if (["r2_put", "r2_list"].includes(name)) this.bump("r2_class_a");
    if (["r2_get", "r2_head"].includes(name)) this.bump("r2_class_b");
    if (name === "r2_delete") this.bump("r2_deletes");
    try {
      const result = await callback();
      op.successes++;
      const received = readBytes(result);
      op.read_bytes += received;
      op.written_bytes += writeBytes;
      if (keyOp) { keyOp.read_bytes += received; keyOp.written_bytes += writeBytes; }
      this.bump("r2_read_bytes", received);
      this.bump("r2_written_bytes", writeBytes);
      return result;
    } catch (error) { op.failures++; if (keyOp) keyOp.failures++; throw error; }
  }

  async inventory(prefix) {
    const objects = [];
    let cursor;
    do {
      const page = await this.operation("r2_list", () => this.env.BLOBS.list({ prefix, limit: 1000, cursor }));
      objects.push(...page.objects);
      if (page.truncated && !page.cursor) throw new Error("truncated R2 list has no cursor");
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor);
    return objects;
  }

  updatePeak() {
    if (this.liveBytes > this.peakBytes) {
      this.peakBytes = this.liveBytes;
      this.exec("INSERT INTO high_water (id, bytes) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET bytes = excluded.bytes", this.peakBytes);
    }
  }

  async remove(key, prefix, knownSize) {
    const catalog = this.exec("SELECT size FROM blob_catalog WHERE key = ?", key)[0];
    let size = knownSize ?? catalog?.size;
    if (size === undefined) {
      const object = await this.operation("r2_head", () => this.env.BLOBS.head(prefix + key), () => 0, 0, key);
      size = object?.size;
    }
    await this.operation("r2_delete", () => this.env.BLOBS.delete(prefix + key), () => 0, 0, key);
    this.exec("DELETE FROM blob_catalog WHERE key = ?", key);
    if (size !== undefined) {
      this.liveObjects = Math.max(0, this.liveObjects - 1);
      this.liveBytes = Math.max(0, this.liveBytes - size);
    }
  }

  async handle(request) {
    this.currentPhase = phase(request);
    this.bump("do_requests");
    const repo = repoName(request.headers.get("x-hosting-repo"));
    const prefix = `${repo}/`;
    const path = new URL(request.url).pathname;
    if (path === "/_services/blobs/read" && request.method === "POST") {
      const body = await parseJson(request, MAX_MANIFEST_BYTES);
      if (!plainObject(body) || !Array.isArray(body.chunks)
          || body.chunks.length === 0 || body.chunks.length > MAX_BULK_KEYS) fail(400, "invalid bulk read shape");
      let wireBytes = BULK_HEADER_BYTES;
      const lengths = new Map();
      // Validate the complete batch, including framing, before any binding I/O.
      for (const chunk of body.chunks) {
        if (!plainObject(chunk) || typeof chunk.key !== "string" || chunk.key.length !== 71 || !BLOB.test(chunk.key)
            || !Number.isSafeInteger(chunk.len) || chunk.len <= 0 || chunk.len > MAX_BULK_CHUNK_BYTES
            || (lengths.has(chunk.key) && lengths.get(chunk.key) !== chunk.len)) fail(400, "invalid bulk read chunk");
        lengths.set(chunk.key, chunk.len);
        wireBytes += BULK_LENGTH_BYTES + chunk.len;
        if (!Number.isSafeInteger(wireBytes) || wireBytes > MAX_BULK_RESPONSE_BYTES) fail(413, "bulk read response too large");
      }
      const parts = [];
      // Preserve scalar binding order and repeated-key semantics. Completed
      // reads are charged before checking their expected length or framing.
      for (const chunk of body.chunks) {
        const data = await this.operation("r2_get", async () => {
          const object = await this.env.BLOBS.get(prefix + chunk.key);
          if (!object) return null;
          try { return new Uint8Array(await object.arrayBuffer()); }
          catch (error) {
            // The consumed body length is unknown. Suppress this receipt so
            // the existing Worker validator marks the accounting incomplete.
            this.accountingComplete = false;
            throw error;
          }
        }, (value) => value?.byteLength ?? 0, 0, chunk.key);
        if (!data) fail(404, "blob not found");
        if (data.byteLength !== chunk.len) fail(409, "blob length differs from bulk read declaration");
        parts.push(data);
      }
      const data = new Uint8Array(wireBytes);
      data.set([0x47, 0x5a, 0x42, 0x52]); // GZBR
      const frame = new DataView(data.buffer);
      frame.setUint32(4, 1, true);
      frame.setUint32(8, parts.length, true);
      let offset = BULK_HEADER_BYTES;
      for (const part of parts) {
        frame.setBigUint64(offset, BigInt(part.byteLength), true);
        offset += BULK_LENGTH_BYTES;
        data.set(part, offset);
        offset += part.byteLength;
      }
      return new Response(data, { headers: { "Content-Type": "application/octet-stream", "Cache-Control": "no-store" } });
    }
    if (path.startsWith("/_services/blob/")) {
      const key = path.slice("/_services/blob/".length);
      if (!BLOB.test(key)) fail(400, "expected a content-addressed chunks/<SHA256> key");
      if (request.method === "GET") {
        const data = await this.operation("r2_get", async () => {
          const object = await this.env.BLOBS.get(prefix + key);
          return object ? new Uint8Array(await object.arrayBuffer()) : null;
        }, (value) => value?.byteLength ?? 0, 0, key);
        return data ? new Response(data, { headers: { "Content-Type": "application/octet-stream" } })
          : json({ error: "blob not found" }, 404);
      }
      if (request.method === "PUT") {
        const data = await bytes(request, MAX_BLOB_BYTES);
        const sha = new Uint8Array(await crypto.subtle.digest("SHA-256", data));
        const hex = Array.from(sha, (byte) => byte.toString(16).padStart(2, "0")).join("");
        if (key !== `chunks/${hex}`) fail(409, "content does not match immutable blob key");
        const previous = this.exec("SELECT size FROM blob_catalog WHERE key = ?", key)[0];
        await this.operation("r2_put", () => this.env.BLOBS.put(prefix + key, data), () => 0, data.byteLength, key);
        this.exec("INSERT INTO blob_catalog (key, size) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET size = excluded.size", key, data.byteLength);
        if (!previous) this.liveObjects++;
        this.liveBytes += data.byteLength - (previous?.size ?? 0);
        this.updatePeak();
        await this.ctx.storage.sync();
        return new Response(null, { status: 204 });
      }
      if (request.method === "DELETE") {
        await this.remove(key, prefix);
        await this.ctx.storage.sync();
        return new Response(null, { status: 204 });
      }
      return json({ error: "method not allowed" }, 405);
    }
    if (path === "/_services/manifest") {
      if (request.method === "GET") {
        return this.operation("manifest_get", async () => {
          const row = this.exec("SELECT manifest FROM repository_state WHERE id = 1")[0];
          return json(row ? JSON.parse(row.manifest) : { generation: 0, refs: {}, packs: [] });
        });
      }
      if (request.method === "POST") {
        const body = await parseJson(request, MAX_MANIFEST_BYTES);
        if (!Number.isSafeInteger(body.expected) || body.expected < 0
            || body.next?.generation !== body.expected + 1 || !Number.isSafeInteger(body.next.generation)
            || !body.next.refs || typeof body.next.refs !== "object" || Array.isArray(body.next.refs)
            || !Array.isArray(body.next.packs)) fail(400, "invalid manifest generation or shape");
        return this.operation("manifest_cas", async () => {
          const published = this.ctx.storage.transactionSync(() => {
            const generation = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0]?.generation ?? 0;
            if (generation !== body.expected) return false;
            this.exec("INSERT INTO repository_state (id, generation, manifest) VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET generation = excluded.generation, manifest = excluded.manifest",
              body.next.generation, JSON.stringify(body.next));
            return true;
          });
          await this.ctx.storage.sync();
          return json({ published });
        });
      }
      return json({ error: "method not allowed" }, 405);
    }
    if (path === "/_services/gc" && request.method === "POST") {
      const body = await parseJson(request, MAX_MANIFEST_BYTES);
      if (!Array.isArray(body.live_keys) || body.live_keys.some((key) => !BLOB.test(key))) fail(400, "invalid GC live keys");
      const generation = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0]?.generation ?? 0;
      if (!Number.isSafeInteger(body.expected_generation) || body.expected_generation < 0) fail(400, "GC requires expected_generation");
      if (body.expected_generation !== generation) fail(409, "stale GC generation");
      const live = new Set(body.live_keys);
      const objects = await this.inventory(prefix);
      let removed = 0;
      for (const object of objects) {
        const key = object.key.slice(prefix.length);
        if (!live.has(key)) { await this.remove(key, prefix, object.size); removed++; }
      }
      // Reconcile catalog state after interrupted put/catalog updates as well.
      this.ctx.storage.transactionSync(() => {
        this.exec("DELETE FROM blob_catalog");
        for (const object of objects) {
          const key = object.key.slice(prefix.length);
          if (live.has(key)) this.exec("INSERT INTO blob_catalog (key, size) VALUES (?, ?)", key, object.size);
        }
      });
      this.liveObjects = objects.filter((object) => live.has(object.key.slice(prefix.length))).length;
      this.liveBytes = objects.filter((object) => live.has(object.key.slice(prefix.length))).reduce((sum, object) => sum + object.size, 0);
      this.updatePeak();
      await this.ctx.storage.sync();
      return json({ deleted: removed });
    }
    if (path === "/_services/metrics" && request.method === "GET") {
      const objects = await this.inventory(prefix);
      const liveBytes = objects.reduce((sum, object) => sum + object.size, 0);
      this.peakBytes = Math.max(this.peakBytes, liveBytes);
      const row = this.exec("SELECT LENGTH(CAST(manifest AS BLOB)) AS bytes FROM repository_state WHERE id = 1")[0];
      const counters = { ...this.totals };
      if (!this.sqlRowsKnown) { counters.do_rows_read = null; counters.do_rows_written = null; }
      return json({
        ...counters, r2_live_objects: objects.length, r2_live_bytes: liveBytes,
        r2_peak_bytes: this.peakBytes, do_sqlite_bytes: this.ctx.storage.sql.databaseSize,
        manifest_bytes: row?.bytes ?? 0, worker_cpu_ms: null, do_gb_seconds: null,
        repository: repo, by_phase: this.byPhase, operations: this.operations,
        r2_key_access: {
          capacity: 256, tracked_keys: this.keyUsage.size, truncated: this.keyUsageTruncated,
          counter_scope: "recent R2 binding calls in this Durable Object instance; aggregate counters remain authoritative",
          top_keys: [...this.keyUsage.values()].sort((a, b) => b.read_bytes - a.read_bytes
            || b.get_attempts - a.get_attempts || b.put_attempts - a.put_attempts
            || a.key.localeCompare(b.key)).slice(0, 10),
        },
        counter_scope: "current Worker runtime / Durable Object instance; R2 inventory persists",
        counter_epoch: this.epoch,
        local_only: true, runtime: "workerd", observation_phase: "stats",
        topology: "Worker gateway -> local Rust Git codec -> Worker service bridge -> Durable Object -> R2/SQLite",
      });
    }
    return json({ error: "unknown service route" }, 404);
  }

  fetch(request) {
    // Serialize only service calls, including their R2 I/O. The gateway never
    // enters this gate while awaiting its local codec, avoiding nested deadlock.
    return this.ctx.blockConcurrencyWhile(async () => {
      // Constructor SQL is charged to the first receipt. Subsequent receipts
      // contain only their own RPC, so the Worker can sum disjoint deltas even
      // when idle eviction reconstructs this class between codec requests.
      if (!this.firstRequest) {
        this.totals = emptyUsage(); this.byPhase = {}; this.operations = {};
        this.sqlRowsKnown = true;
      } else {
        // The request that cold-starts the object incurs its constructor SQL.
        // An observer-triggered reconstruction therefore stays in stats.
        this.byPhase = { [phase(request)]: { ...this.totals } };
      }
      this.firstRequest = false;
      this.accountingComplete = true;
      let response;
      try { response = await this.handle(request); }
      catch (error) { response = json({ error: String(error.message ?? error) }, error.status ?? 500); }
      const totals = { ...this.totals };
      const byPhase = structuredClone(this.byPhase);
      if (!this.sqlRowsKnown) {
        totals.do_rows_read = null; totals.do_rows_written = null;
        for (const counters of Object.values(byPhase)) { counters.do_rows_read = null; counters.do_rows_written = null; }
      }
      const receipt = { version: 1, do_instance_epoch: this.epoch, totals,
        by_phase: byPhase, operations: this.operations };
      const headers = new Headers(response.headers);
      if (this.accountingComplete) headers.set(USAGE_HEADER, JSON.stringify(receipt));
      return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
    });
  }
}
