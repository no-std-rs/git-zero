// Local workerd runtime glue. Git and all test/benchmark runners remain Rust.
// Never deploy this loopback codec gateway as a production Git service.
import { PAGE_PREFIX, PAGE_END, PAGE_CAPACITY, ancestors, commonPrefix, count, decodePage, encodePage, splitPage } from "./catalog-pages.js";
import { PROOF_HEADER, DEFERRED_HEADER, CONFIRM_HEADER, DEFERRED_CAPABILITY, debtDigest, registrationProof, issueProof, verifyProof } from "./retirement-proof.js";
import { INLINE_HEADER, INLINE_CAPABILITY, encodeInline, decodeInline, inlinePeak } from "./inline-catalog.js";
import { registrationFirstRead, eligibleFirstRead, gatewayFirstRead, FETCH_BODY_MAX, validateFirstBatch, manifestFingerprint } from "./first-read.js";
import { SOCKET_CAPABILITY, socketRegistration, closeRegisteredSocket, upgradeServiceSocket } from "./service-socket.js";
const registry = new Map();
const workerUsage = new Map();
let workerEpoch;
const PHASES = new Set(["startup", "push", "fetch", "maintenance", "stats"]);
const REPO = /^[A-Za-z0-9_-]{1,80}$/;
const BLOB = /^chunks\/[0-9a-f]{64}$/;
const MAX_BLOB_BYTES = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES = 1_500_000;
const MAX_RETIREMENT_KEYS = 256;
const MAX_SNAPSHOT_BYTES = 128 * 1024 * 1024;
const MAX_BULK_KEYS = 256;
const MAX_BULK_CHUNK_BYTES = 16 * 1024 * 1024;
const MAX_BULK_PAYLOAD_BYTES = 16 * 1024 * 1024;
const BULK_HEADER_BYTES = 12;
const BULK_LENGTH_BYTES = 8;
const MAX_BULK_RESPONSE_BYTES = MAX_BULK_PAYLOAD_BYTES + BULK_HEADER_BYTES + BULK_LENGTH_BYTES * MAX_BULK_KEYS;
const UPLOAD_CAPABILITY = { version: 1, wire_bytes: MAX_BULK_RESPONSE_BYTES,
  metadata_bytes: MAX_MANIFEST_BYTES, chunk_bytes: MAX_BLOB_BYTES, retirement_keys: MAX_RETIREMENT_KEYS };
const WITNESS_PREFIX = "x-git-zero-internal-";
const WITNESS_HEADER = `${WITNESS_PREFIX}generation-witness`;
const CATALOG_PEAK_HEADER = `${WITNESS_PREFIX}catalog-peak`;
const CATALOG_PAGES_HEADER = `${WITNESS_PREFIX}catalog-pages`;
const WITNESS_DOMAIN = "git-zero-generation-witness";
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

function validManifestCas(body) {
  return plainObject(body) && Number.isSafeInteger(body.expected) && body.expected >= 0
    && body.next?.generation === body.expected + 1 && Number.isSafeInteger(body.next.generation)
    && plainObject(body.next.refs) && Array.isArray(body.next.packs);
}

// Validate the entire fused request before publication or retirement I/O. The
// durable manifest keeps all debt, including keys shared with live packs.
function retirementKeys(body) {
  if (!plainObject(body) || body.version !== 1 || !validManifestCas(body)
      || Object.values(body.next.refs).some((value) => typeof value !== "string")) fail(400, "invalid retiring manifest");
  const garbage = body.next.garbage === undefined ? [] : body.next.garbage;
  if (!Array.isArray(garbage) || garbage.length > MAX_RETIREMENT_KEYS) fail(400, "invalid retirement debt bound");
  const debt = new Set();
  for (const key of garbage) {
    if (typeof key !== "string" || key.length !== 71 || !BLOB.test(key) || debt.has(key)) fail(400, "invalid retirement key");
    debt.add(key);
  }
  const ids = new Set(), live = new Map();
  let snapshotBytes = 0;
  const chunks = (items, expected) => {
    if (!Array.isArray(items)) fail(400, "invalid retiring chunk shape");
    let total = 0;
    for (const chunk of items) {
      if (!plainObject(chunk) || typeof chunk.key !== "string" || chunk.key.length !== 71 || !BLOB.test(chunk.key)
          || !Number.isSafeInteger(chunk.len) || chunk.len <= 0 || chunk.len > MAX_SNAPSHOT_BYTES
          || (live.has(chunk.key) && live.get(chunk.key) !== chunk.len)) fail(400, "invalid retiring chunk");
      live.set(chunk.key, chunk.len);
      total += chunk.len;
      if (!Number.isSafeInteger(total) || total > MAX_SNAPSHOT_BYTES) fail(400, "invalid retiring chunk bound");
    }
    if (total !== expected) fail(400, "invalid retiring chunk lengths");
  };
  for (const pack of body.next.packs) {
    if (!plainObject(pack) || typeof pack.id !== "string" || ids.has(pack.id)
        || !Number.isSafeInteger(pack.level) || pack.level < 0 || pack.level > 0xffffffff
        || !Number.isSafeInteger(pack.pack_bytes) || pack.pack_bytes < 0
        || !Number.isSafeInteger(pack.index_bytes) || pack.index_bytes < 0
        || !Array.isArray(pack.pack) || !Array.isArray(pack.index)
        || (pack.bundle !== undefined && !Array.isArray(pack.bundle))) fail(400, "invalid retiring pack shape");
    ids.add(pack.id);
    const total = pack.pack_bytes + pack.index_bytes;
    snapshotBytes += total;
    if (!Number.isSafeInteger(snapshotBytes) || snapshotBytes > MAX_SNAPSHOT_BYTES) fail(400, "invalid retiring snapshot bound");
    if (pack.bundle?.length) {
      if (pack.pack.length || pack.index.length) fail(400, "mixed retiring pack representations");
      chunks(pack.bundle, total);
    } else {
      chunks(pack.pack, pack.pack_bytes);
      chunks(pack.index, pack.index_bytes);
    }
  }
  return garbage.filter((key) => !live.has(key));
}

function exactFields(value, required, optional = []) {
  return plainObject(value) && required.every((key) => Object.hasOwn(value, key))
    && Object.keys(value).every((key) => required.includes(key) || optional.includes(key));
}

// JSON.parse alone discards duplicate object members. Scan already-valid JSON
// tokens as well, including decoded/escaped names, before accepting metadata.
function strictJson(data) {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(data);
    const value = JSON.parse(text);
    const tokens = text.match(/"(?:[^"\\]|\\[\s\S])*"|[{}\[\],:]|[^\s{}\[\],:]+/g) ?? [];
    let position = 0;
    const scan = (depth) => {
      if (depth > 32) fail(400, "metadata nesting too deep");
      const token = tokens[position++];
      if (token === "{") {
        const names = new Set();
        if (tokens[position] !== "}") for (;;) {
          const name = JSON.parse(tokens[position++]);
          if (names.has(name)) fail(400, "duplicate metadata field");
          names.add(name);
          position++; // colon, already checked by JSON.parse
          scan(depth + 1);
          if (tokens[position++] === "}") return;
        }
        position++;
      } else if (token === "[") {
        if (tokens[position] !== "]") for (;;) {
          scan(depth + 1);
          if (tokens[position++] === "]") return;
        }
        position++;
      }
    };
    scan(0);
    if (position !== tokens.length) fail(400, "invalid metadata tokens");
    return value;
  } catch (error) { if (error.status) throw error; fail(400, "invalid upload JSON"); }
}

async function uploadFrame(request) {
  const frame = await bytes(request, MAX_BULK_RESPONSE_BYTES);
  if (frame.byteLength < 12 || frame[0] !== 0x47 || frame[1] !== 0x5a || frame[2] !== 0x55 || frame[3] !== 0x50) fail(400, "invalid upload header");
  const header = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
  const metadataBytes = header.getUint32(8, true);
  if (header.getUint32(4, true) !== 1 || metadataBytes > MAX_MANIFEST_BYTES
      || metadataBytes > frame.byteLength - 12) fail(400, "invalid upload metadata bound");
  const body = strictJson(frame.subarray(12, 12 + metadataBytes));
  if (!exactFields(body, ["version", "expected", "next", "chunk"])
      || !exactFields(body.chunk, ["key", "len"])
      || !exactFields(body.next, ["generation", "refs", "packs"], ["garbage"])) fail(400, "invalid upload fields");
  const keys = retirementKeys(body);
  const chunk = body.chunk;
  if (typeof chunk.key !== "string" || chunk.key.length !== 71 || !BLOB.test(chunk.key)
      || !Number.isSafeInteger(chunk.len) || chunk.len <= 0 || chunk.len > MAX_BLOB_BYTES
      || frame.byteLength !== 12 + metadataBytes + chunk.len) fail(400, "invalid upload payload bound");
  let member = false;
  for (const pack of body.next.packs) {
    if (!exactFields(pack, ["id", "pack", "index", "pack_bytes", "index_bytes", "level"], ["bundle"])) fail(400, "unknown upload pack field");
    for (const item of [...pack.pack, ...pack.index, ...(pack.bundle ?? [])]) {
      if (!exactFields(item, ["key", "len"])) fail(400, "unknown upload chunk field");
      if (item.key === chunk.key) {
        if (item.len !== chunk.len) fail(400, "upload member length mismatch");
        member = true;
      }
    }
  }
  if (!member) fail(400, "upload is not a manifest member");
  const refs = Object.entries(body.next.refs);
  if (refs.length > 16 * 1024 || refs.reduce((sum, [name, id]) => sum + new TextEncoder().encode(name + id).byteLength, 0) > 4 * 1024 * 1024
      || refs.some(([name, id]) => new TextEncoder().encode(name).byteLength > 65520 - 4 - 40 - 2 || !name.startsWith("refs/") || name.endsWith("/") || name.endsWith(".")
        || name.includes("..") || name.includes("@{") || /[\x00-\x20\x7f~^:?*\[\\]/.test(name)
        || name.split("/").some((part) => !part || part.startsWith(".") || part.endsWith(".lock"))
        || !/^[0-9a-fA-F]{40}$/.test(id) || /^0{40}$/.test(id))) fail(400, "invalid upload refs");
  const data = frame.subarray(12 + metadataBytes);
  if (chunk.key !== `chunks/${hex(new Uint8Array(await crypto.subtle.digest("SHA-256", data)))}`) fail(409, "upload content does not match key");
  return { body, keys, data };
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
  const receipt = strictJson(new TextEncoder().encode(encoded));
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
  return receipt;
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
  result.socket_rpc = usage.socket_rpc ?? {};
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

// Both callback and gateway reads use exactly the same receipt validation.
// This helper dispatches one DO RPC, never another Worker service request.
async function dispatchService(env, repo, request, requireReceipt = false) {
  const stub = env.REPOSITORIES.get(env.REPOSITORIES.idFromName(repo));
  let response;
  try { response = await stub.fetch(request); }
  catch (error) { incomplete(repo, "Durable Object dispatch failed; side effects may be unaccounted"); throw error; }
  let receipt = null;
  try { receipt = acceptReceipt(repo, response.headers.get(USAGE_HEADER)); }
  catch {
    incomplete(repo, "missing or invalid Durable Object usage receipt");
    if (requireReceipt) fail(502, "service call has no valid usage receipt");
  }
  const headers = new Headers(response.headers);
  headers.delete(USAGE_HEADER);
  return { response: new Response(response.body, { status: response.status, statusText: response.statusText, headers }), receipt };
}

function hex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function registrationWitness() {
  const rawKey = crypto.getRandomValues(new Uint8Array(32));
  const session = hex(crypto.getRandomValues(new Uint8Array(16)));
  const key = await crypto.subtle.importKey("raw", rawKey, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return { private: { key, session }, response: { version: 1, key: hex(rawKey), session } };
}

async function signedGenerationWitness(repo, method, target, config, generation) {
  if (!validCount(generation)) fail(502, "invalid authoritative generation");
  const message = JSON.stringify([WITNESS_DOMAIN, 1, config.session, repo, method, target, generation]);
  const signature = await crypto.subtle.sign("HMAC", config.key, new TextEncoder().encode(message));
  return `1:${config.session}:${generation}:${hex(new Uint8Array(signature))}`;
}

async function generationWitness(env, repo, currentPhase, method, target, config) {
  // Construct this request from trusted routing inputs; public headers cannot
  // choose the repository, phase or route of the authoritative read.
  const { response } = await dispatchService(env, repo, new Request("https://local/_services/manifest", {
    headers: { "x-hosting-repo": repo, "x-hosting-phase": currentPhase },
  }), true);
  if (!response.ok) fail(502, "authoritative generation read failed");
  const generation = (await response.json()).generation;
  return signedGenerationWitness(repo, method, target, config, generation);
}

// HTTP and authenticated socket RPCs share this exact service bridge. Socket
// messages call it internally; they never enter fetch() or count as Workers.
async function bridgeService(request, env, repo, currentPhase, observer = false, expectedRegistration = null) {
  const url = new URL(request.url);
  if (expectedRegistration && registry.get(repo) !== expectedRegistration) fail(409, "socket registration changed");
  const headers = new Headers(request.headers);
  const registration = registry.get(repo), proof = request.headers.get(PROOF_HEADER);
  if (proof && proof.length > 512) fail(400, "retirement proof exceeds header limit");
  headers.delete(PROOF_HEADER);
  for (const name of [...headers.keys()]) {
    if (name.startsWith(WITNESS_PREFIX)) headers.delete(name);
  }
  if (registration?.catalogPeak) headers.set(CATALOG_PEAK_HEADER, "1");
  if (registration?.catalogPages) headers.set(CATALOG_PAGES_HEADER, "1");
  if (registration?.catalogInline) headers.set(INLINE_HEADER, "1");
  if (registration?.proof) headers.set(DEFERRED_HEADER, "1");
  if (!observer && proof) {
    let confirmation = null;
    try { confirmation = await verifyProof(registration?.proof, repo, proof); } catch { /* Paid debt recovery remains available. */ }
    if (confirmation && registry.get(repo) === registration) headers.set(CONFIRM_HEADER, confirmation);
  }
  headers.set("x-hosting-repo", repo);
  headers.set("x-hosting-phase", currentPhase);
  // Bound and finish this small JSON upload before entering the DO gate.
  // Returning a DO error while its forwarded upload is still active can
  // leave workerd's request pump holding the repository gate indefinitely.
  const upload = url.pathname === "/_services/manifest/upload-retire";
  const buffered = (upload || ["/_services/blobs/read", "/_services/manifest/retire"].includes(url.pathname)) && request.method === "POST";
  const frame = buffered ? await bytes(request, upload ? MAX_BULK_RESPONSE_BYTES : MAX_MANIFEST_BYTES) : null;
  let context = null;
  if (registration?.proof && request.method === "POST" && (upload || url.pathname === "/_services/manifest/retire")) {
    let body;
    if (upload) {
      if (frame.length < 12) fail(400, "invalid upload header");
      const length = new DataView(frame.buffer, frame.byteOffset, frame.byteLength).getUint32(8, true);
      if (length > MAX_MANIFEST_BYTES || length > frame.length - 12) fail(400, "invalid upload metadata bound");
      body = strictJson(frame.subarray(12, 12 + length));
    } else body = strictJson(frame);
    context = { generation: body.next?.generation, keys: retirementKeys(body), ...(upload ? { uploaded: body.chunk } : {}) };
  }
  if (expectedRegistration && registry.get(repo) !== expectedRegistration) fail(409, "socket registration changed");
  const serviceRequest = buffered ? new Request(request.url, { method: request.method, headers, body: frame }) : new Request(request, { headers });
  const { response: clean, receipt } = await dispatchService(env, repo, serviceRequest, upload);
  if (expectedRegistration && registry.get(repo) !== expectedRegistration) fail(409, "socket registration changed after dispatch");
  if (context && clean.ok && registry.get(repo) === registration) {
    // Actual receipts are already charged. Optional crypto/body failure
    // suppresses the proof, preserving the authoritative publication.
    try {
      const reply = strictJson(new Uint8Array(await clean.clone().arrayBuffer()));
      const token = await issueProof(registration.proof, repo, context, reply, receipt);
      if (token && registry.get(repo) === registration) clean.headers.set(PROOF_HEADER, token);
    } catch { /* No proof: bounded actual HEADs can recover after eviction. */ }
  }
  if (observer && clean.ok) return json(mergeWorkerUsage(await clean.json(), repo));
  return clean;
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
        if (url.pathname === "/_services/socket") {
          const usage = workerUsage.get(repo);
          usage.socket_rpc ??= {};
          usage.socket_rpc.upgrade_attempts = addCount(usage.socket_rpc.upgrade_attempts ?? 0, 1);
        }
        if (!env.LOCAL_TOKEN || request.headers.get("x-hosting-token") !== env.LOCAL_TOKEN) {
          if (url.pathname === "/_services/socket") {
            const counters = workerUsage.get(repo).socket_rpc;
            counters.auth_rejected = addCount(counters.auth_rejected ?? 0, 1);
          }
          return json({ error: "invalid local service token" }, 401);
        }
        if (url.pathname === "/_services/register" && request.method === "POST") {
          const body = await parseJson(request, 1024);
          const origin = loopbackOrigin(body.codec_origin);
          const witness = body.generation_witness === 1 ? await registrationWitness() : null;
          const firstRead = witness && body.gateway_first_read === 1 ? await registrationFirstRead(witness.response.session) : null;
          const firstReceive = firstRead && body.gateway_first_receive === 1;
          if (firstRead) firstRead.private.receive = !!firstReceive;
          const catalogDeferred = body.catalog_deferred === 1 && body.catalog_pages === 1 && body.catalog_peak === 1;
          const proof = catalogDeferred ? await registrationProof() : null;
          // Recheck after asynchronous key creation so concurrent registrations
          // cannot replace a different origin or exceed the bounded registry.
          if (registry.has(repo) && registry.get(repo).origin !== origin) {
            fail(409, "a different local codec is already registered; unregister it first");
          }
          if (!registry.has(repo) && registry.size >= 256) fail(429, "local codec registry limit reached");
          const catalogPages = body.catalog_pages === 1 && body.catalog_peak === 1;
          const catalogInline = body.catalog_inline === 1 && catalogDeferred && body.manifest_upload_retire === 1;
          closeRegisteredSocket(registry.get(repo));
          const socket = body.service_socket === 1 ? socketRegistration() : null;
          registry.set(repo, { socket, origin, firstRead: firstRead?.private, witness: witness?.private, catalogPeak: body.catalog_peak === 1, catalogPages, proof, catalogInline });
          return json({ registered: true, repository: repo, codec_origin: origin,
            ...(body.manifest_retire === 1 ? { manifest_retire: { version: 1 } } : {}),
            ...(body.manifest_upload_retire === 1 ? { manifest_upload_retire: UPLOAD_CAPABILITY } : {}),
            ...(body.catalog_peak === 1 ? { catalog_peak: { version: 1 } } : {}),
            ...(catalogPages ? { catalog_pages: { version: 1 } } : {}),
            ...(proof ? { catalog_deferred: DEFERRED_CAPABILITY } : {}),
            ...(catalogInline ? { catalog_inline: INLINE_CAPABILITY } : {}),
            ...(firstRead ? { gateway_first_read: firstRead.response } : {}),
            ...(firstReceive ? { gateway_first_receive: { version: 1, body_bytes: FETCH_BODY_MAX, frame_bytes: firstRead.response.frame_bytes, session: firstRead.response.session } } : {}),
            ...(witness ? { generation_witness: witness.response } : {}),
            ...(socket ? { service_socket: { ...SOCKET_CAPABILITY, session: socket.session } } : {}) });
        }
        if (url.pathname === "/_services/unregister" && request.method === "POST") {
          closeRegisteredSocket(registry.get(repo));
          registry.delete(repo);
          return json({ registered: false, repository: repo });
        }
        if (url.pathname === "/_services/socket") {
          const registration = registry.get(repo);
          return upgradeServiceSocket(request, {
            repo, registration, current: () => registry.get(repo) === registration,
            strictJson, bytes, json, fail,
            diagnostic: (name, count = 1) => {
              const usage = workerUsage.get(repo);
              usage.socket_rpc ??= {};
              usage.socket_rpc[name] = addCount(usage.socket_rpc[name] ?? 0, count);
            },
            dispatch: (internal, rpcPhase) => bridgeService(internal, env, repo, rpcPhase, false, registration),
          });
        }
        return await bridgeService(request, env, repo, currentPhase, observer);
      }
      const forwardedPath = prefixed ? url.pathname.slice(repo.length + 1) : url.pathname;
      if (!forwardedPath.startsWith("/repo.git/")) return json({ error: "unknown route" }, 404);
      const registered = registry.get(repo);
      const origin = registered?.origin ?? env.CODEC_ORIGIN;
      if (!origin) return json({ error: "register a local Rust codec first" }, 503);
      const gitPhase = forwardedPath.endsWith("/git-receive-pack")
        || url.searchParams.get("service") === "git-receive-pack" ? "push" : "fetch";
      workerCount(repo, "gateway", gitPhase);
      const headers = new Headers(request.headers);
      headers.delete("host");
      headers.delete("x-hosting-token");
      headers.delete(PROOF_HEADER);
      for (const name of [...headers.keys()]) {
        if (name.startsWith(WITNESS_PREFIX)) headers.delete(name);
      }
      headers.set("x-hosting-repo", repo);
      headers.set("x-hosting-phase", gitPhase);
      const forwardedTarget = forwardedPath + url.search;
      const target = loopbackOrigin(origin) + forwardedTarget;
      let forwardedBody = request.body;
      if (eligibleFirstRead(request, forwardedTarget, registered, headers)) {
        const body = await bytes(request, FETCH_BODY_MAX);
        if (registry.get(repo) !== registered) fail(502, "registration changed while buffering fetch");
        const result = await gatewayFirstRead({repo,target:forwardedTarget,headers,body,registration:registered,
          current:()=>registry.get(repo)===registered,strictJson,
          dispatch:async (internal)=> (await dispatchService(env,repo,internal,true)).response,
          witness:(generation)=>signedGenerationWitness(repo,request.method,forwardedTarget,registered.witness,generation)});
        if (result) return result;
        forwardedBody = body;
      }
      if (registered?.witness) {
        // Any prefetch/signing failure returns an error. Never silently issue
        // a second callback after completed, receipt-backed DO work.
        headers.set(WITNESS_HEADER, await generationWitness(
          env, repo, gitPhase, request.method, forwardedTarget, registered.witness));
      }
      // This awaits Rust outside a Durable Object. Rust can safely call back
      // into /_services while its Git request is in flight.
      return await fetch(new Request(target, {
        method: request.method, headers, body: forwardedBody, redirect: "manual",
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
    this.peakBytes = 0;
    this.catalogCertain = true;
    this.peakOwner = null;
    this.debtLoaded = false;
    this.debtContext = null;
    this.debt = null;
    this.deferredPages = false;
    this.deferAllowed = false;
    this.reloadCatalog();
  }

  bump(name, value = 1) {
    this.totals[name] += value;
    this.byPhase[this.currentPhase] ??= emptyUsage();
    this.byPhase[this.currentPhase][name] += value;
  }

  exec(statement, ...bindings) {
    let cursor, result;
    try {
      cursor = this.ctx.storage.sql.exec(statement, ...bindings);
      result = cursor.toArray();
    } catch (error) {
      // A failed cursor provides no authoritative row counts. Preserve actual
      // binding receipts while making the SQL component explicitly unknown.
      this.sqlRowsKnown = false;
      throw error;
    }
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

  reloadCatalog() {
    // Reload discards volatile deltas together with their exact live totals.
    // Reclassify from durable current debt; a surviving in-memory confirmation
    // must never be subtracted twice or applied to newly loaded stale totals.
    this.debtLoaded = false;
    this.debtContext = null;
    this.debt = null;
    const stateColumns = this.exec("PRAGMA table_info(repository_state)");
    this.inlineSchema = stateColumns.some((column) => column.name === "catalog_root");
    this.inlineActive = false; this.inlineRoot = null;
    const columns = this.exec("PRAGMA table_info(blob_catalog)");
    this.catalogPeak = columns.some((column) => column.name === "peak_bytes");
    this.catalogPages = columns.some((column) => column.name === "entries");
    if (this.catalogPages && !this.catalogPeak) throw new Error("catalog pages require durable peak schema");
    const validSize = "typeof(size) = 'integer' AND size BETWEEN 0 AND 9007199254740991";
    const validPeak = "typeof(peak_bytes) = 'integer' AND peak_bytes BETWEEN 0 AND 9007199254740991";
    const v1 = "hex(substr(entries, 1, 5)) = '475A435001' AND LENGTH(entries) BETWEEN 46 AND 1286 AND (LENGTH(entries) - 6) % 40 = 0 AND hex(substr(entries, 6, 1)) = printf('%02X', (LENGTH(entries) - 6) / 40)";
    const v2 = "hex(substr(entries, 1, 5)) = '475A435002' AND LENGTH(entries) BETWEEN 50 AND 1166 AND (LENGTH(entries) - 14) % 36 = 0 AND hex(substr(entries, 6, 1)) = printf('%02X', (LENGTH(entries) - 14) / 36)";
    const inventory = this.exec(this.catalogPages
      ? `SELECT COALESCE(SUM(CASE WHEN entries IS NULL THEN 1 WHEN ${v1} THEN (LENGTH(entries) - 6) / 40 WHEN ${v2} THEN (LENGTH(entries) - 14) / 36 ELSE 0 END), 0) AS objects, COALESCE(SUM(CASE WHEN ${validSize} THEN 1.0 * size ELSE 0 END), 0) AS bytes, COALESCE(MAX(CASE WHEN ${validPeak} THEN peak_bytes ELSE 0 END), 0) AS peak, COALESCE(MAX(CASE WHEN NOT (${validSize}) OR NOT (${validPeak}) OR (entries IS NOT NULL AND NOT ((${v1}) OR (${v2}))) THEN 1 ELSE 0 END), 0) AS invalid, COALESCE(MAX(CASE WHEN ${v2} THEN 1 ELSE 0 END), 0) AS deferred FROM blob_catalog`
      : this.catalogPeak
      ? "SELECT COUNT(*) AS objects, COALESCE(SUM(size), 0) AS bytes, COALESCE(MAX(peak_bytes), 0) AS peak FROM blob_catalog"
      : "SELECT COUNT(*) AS objects, COALESCE(SUM(size), 0) AS bytes FROM blob_catalog")[0];
    const fallback = this.exec("SELECT bytes FROM high_water WHERE id = 1")[0]?.bytes ?? 0;
    const valid = (value) => Number.isSafeInteger(value) && value >= 0;
    // REAL summation avoids SQLite integer overflow before validation. Within
    // the safe nonnegative integer range every partial sum remains exact.
    // Invalid rows/totals cannot certify a live inventory or hide valid H.
    const inventoryValid = !inventory.invalid && valid(inventory.objects) && valid(inventory.bytes);
    this.fallbackValid = valid(fallback);
    if (this.catalogPages && (!inventoryValid || !this.fallbackValid)) this.catalogUncertain();
    this.liveObjects = this.catalogPages && !inventoryValid ? 0 : count(inventory.objects);
    this.liveBytes = this.catalogPages && !inventoryValid ? 0 : count(inventory.bytes);
    this.fallbackPeak = this.catalogPages && !this.fallbackValid ? 0 : count(fallback);
    this.peakBytes = Math.max(this.peakBytes, this.liveBytes, count(inventory.peak ?? 0), this.fallbackPeak);
    this.peakOwner = null;
    this.deferredPages = !!inventory.deferred;
    if (this.inlineSchema) {
      const state = this.exec("SELECT generation, catalog_root FROM repository_state WHERE id = 1")[0];
      if (state?.catalog_root !== null && state?.catalog_root !== undefined) {
        this.inlineActive = true;
        this.peakBytes = Math.max(this.peakBytes, inlinePeak(state.catalog_root));
        try {
          this.inlineRoot = decodeInline(state.catalog_root, state.generation);
          if (inventory.objects !== 0 || !inventoryValid || !this.catalogPages) throw new Error("duplicate or unsupported inline ownership");
          this.liveObjects = this.inlineRoot.entries.length;
          this.liveBytes = this.inlineRoot.size;
          this.peakBytes = Math.max(this.peakBytes, this.inlineRoot.peak, this.liveBytes);
          this.deferredPages = true;
        } catch { this.catalogUncertain(); }
      }
    }
    // Durable format, never the ephemeral registration, selects reopen logic.
    if (this.deferredPages && !this.debtLoaded && this.catalogCertain) {
      try { this.loadDebtContext(); } catch { this.catalogUncertain(); }
    }
    // SQL recovers peak witnesses, never certainty about an interrupted R2
    // operation. Only a generation-checked physical inventory clears dirty.
  }

  requireCatalogCertain() {
    if (!this.catalogCertain) fail(409, "physical inventory reconciliation required before mutation");
  }

  catalogUncertain() {
    this.catalogCertain = false;
    this.peakOwner = null;
  }

  setDebtContext(manifest) {
    count(manifest.generation);
    if (Array.isArray(manifest.garbage) && manifest.garbage.length > MAX_RETIREMENT_KEYS) {
      // Native deliberately uses scalar publication/deletion above the fused
      // bound. That path is eager, not invalid: retain only its generation and
      // never build an oversized proof/debt cache or infer physical absence.
      this.debtContext = { generation: manifest.generation, keys: [], eager: true };
      this.debtLoaded = true;
      return;
    }
    const keys = manifest.generation === 0 ? [] : retirementKeys({ version: 1, expected: manifest.generation - 1, next: manifest });
    this.debtContext = { generation: manifest.generation, keys };
    this.debtLoaded = true;
  }

  loadDebtContext() {
    const row = this.exec("SELECT manifest FROM repository_state WHERE id = 1")[0];
    this.setDebtContext(row ? JSON.parse(row.manifest) : { generation: 0, refs: {}, packs: [] });
    const { generation, keys } = this.debtContext, items = [];
    for (const key of keys) {
      const found = this.catalogLookup(key);
      const appliedGeneration = found.entries?.appliedGeneration ?? null;
      if (appliedGeneration !== null && appliedGeneration > generation) throw new Error("catalog acknowledgment exceeds manifest generation");
      if (found.size !== undefined && (appliedGeneration === null || appliedGeneration < generation)) {
        items.push({ key, rowKey: found.legacy?.key ?? found.page.key, size: found.size, appliedGeneration, absent: false });
      }
    }
    this.debt = items.length ? { generation, keys, items, resolved: false } : null;
  }

  async resolveDebt(prefix) {
    this.requireCatalogCertain();
    if (!this.catalogPages || (!this.deferredPages && !this.deferAllowed)) return;
    if (!this.debtLoaded) {
      try { this.loadDebtContext(); } catch (error) { this.catalogUncertain(); throw error; }
    }
    if (!this.debt || this.debt.resolved) return;
    let confirmed = false;
    try {
      const proof = JSON.parse(this.confirmation ?? "null");
      confirmed = Array.isArray(proof) && proof.length === 3 && proof[0] === 1
        && proof[1] === this.debt.generation && proof[2] === await debtDigest(this.debt.keys);
    } catch { /* Missing/stale proof uses actual bounded binding calls. */ }
    // Accumulate first. A failed HEAD leaves the original bounded uncertainty
    // unresolved; it cannot partially change the exact live counters.
    for (const item of this.debt.items) {
      if (confirmed && item.appliedGeneration !== null) item.absent = true;
      else {
        const object = await this.operation("r2_head", () => this.env.BLOBS.head(prefix + item.key), () => 0, 0, item.key);
        if (object && object.size !== item.size) { this.catalogUncertain(); throw new Error("debt member size differs from physical object"); }
        item.absent = !object;
      }
    }
    for (const item of this.debt.items) if (item.absent) {
      this.liveObjects = count(this.liveObjects - 1);
      this.liveBytes = count(this.liveBytes - item.size);
    }
    this.debt.resolved = true;
  }

  async flushDebt() {
    if (!this.debt) return;
    this.requireCatalogCertain();
    if (!this.debt.resolved) throw new Error("retirement debt must be reconciled before persistence");
    try {
      const groups = new Map();
      for (const item of this.debt.items) {
        if (!groups.has(item.rowKey)) groups.set(item.rowKey, []);
        groups.get(item.rowKey).push(item);
      }
      const doomed = new Set();
      for (const [key, items] of groups) {
        const row = this.catalogRow(key);
        if (!row) throw new Error("pending catalog row disappeared");
        const members = row.entries === null ? 1 : this.decodeCatalogRow(row).length;
        if (items.filter((item) => item.absent).length === members) doomed.add(key);
      }
      await this.preserveBatchPeak(doomed);
      this.ctx.storage.transactionSync(() => {
        for (const [key, items] of groups) {
          if (!key.startsWith(PAGE_PREFIX)) {
            if (items[0].absent) this.exec("DELETE FROM blob_catalog WHERE key = ?", key);
            continue;
          }
          const row = this.catalogRow(key);
          const entries = this.decodeCatalogRow(row);
          if (entries.appliedGeneration !== null && entries.appliedGeneration >= this.debt.generation) continue;
          const absent = new Set(items.filter((item) => item.absent).map((item) => item.key.slice(7)));
          const survivors = entries.filter((entry) => !absent.has(entry.digest));
          if (survivors.length || this.inlineActive) this.writePage(key, survivors, row.peak_bytes, this.debt.generation);
          else this.exec("DELETE FROM blob_catalog WHERE key = ?", key);
        }
      });
      await this.ctx.storage.sync();
      this.debt = null;
      this.peakOwner = null;
    } catch (error) { this.catalogUncertain(); throw error; }
  }

  enableCatalogPeak(request, reconciling = false) {
    if (!reconciling) this.requireCatalogCertain();
    if (!this.catalogPeak && request.headers.get(CATALOG_PEAK_HEADER) === "1") {
      try {
        this.exec("ALTER TABLE blob_catalog ADD COLUMN peak_bytes INTEGER NOT NULL DEFAULT 0");
        this.catalogPeak = true;
      } catch (error) {
        this.catalogUncertain();
        throw error;
      }
    }
    if (!this.catalogPages && request.headers.get(CATALOG_PAGES_HEADER) === "1") {
      try {
        if (!this.catalogPeak) throw new Error("catalog pages require durable peak schema");
        this.exec("ALTER TABLE blob_catalog ADD COLUMN entries BLOB");
        this.catalogPages = true;
      } catch (error) { this.catalogUncertain(); throw error; }
    }
  }

  catalogRow(key) {
    if (this.inlineActive && key === PAGE_PREFIX) {
      if (!this.inlineRoot) throw new Error("invalid inline catalog owner");
      return { key, size: this.inlineRoot.size, peak_bytes: this.inlineRoot.peak,
        entries: encodeInline(this.inlineRoot.entries, this.inlineRoot.peak, this.inlineRoot.appliedGeneration) };
    }
    return this.exec("SELECT key, size, peak_bytes, entries FROM blob_catalog WHERE key = ?", key)[0];
  }

  decodeCatalogRow(row) {
    if (this.inlineActive && row.key === PAGE_PREFIX) {
      const entries = this.inlineRoot.entries.map((entry) => ({ ...entry }));
      entries.appliedGeneration = this.inlineRoot.appliedGeneration;
      return entries;
    }
    return decodePage(row);
  }

  writeInline(entries, peak, appliedGeneration) {
    const blob = encodeInline(entries, peak, appliedGeneration);
    this.exec("UPDATE repository_state SET catalog_root = ? WHERE id = 1", blob);
    this.inlineRoot = decodeInline(blob, Number.MAX_SAFE_INTEGER);
    this.deferredPages = true;
  }

  async demoteInline() {
    if (!this.inlineActive) return;
    this.requireCatalogCertain();
    await this.resolveDebt(this.repoPrefix);
    await this.flushDebt();
    const root = this.inlineRoot;
    // Count-zero is a real owner. Transfer its H before clearing it.
    if (!root.entries.length) await this.checkpointPeak(Math.max(root.peak, this.peakBytes));
    try {
      this.ctx.storage.transactionSync(() => {
        this.exec("UPDATE repository_state SET catalog_root = NULL WHERE id = 1");
        this.inlineActive = false;
        if (root.entries.length) this.writePage(PAGE_PREFIX, root.entries, root.peak, root.appliedGeneration);
      });
      await this.ctx.storage.sync();
      this.inlineRoot = null;
      this.peakOwner = null;
    } catch (error) { this.catalogUncertain(); throw error; }
  }

  async prepareInline(next, key) {
    if (!this.inlineAllowed || !this.catalogPages || !this.deferAllowed) return false;
    this.requireCatalogCertain();
    await this.resolveDebt(this.repoPrefix);
    if (new TextEncoder().encode(JSON.stringify(next)).length > INLINE_CAPABILITY.manifest_bytes) {
      await this.demoteInline();
      return false;
    }
    if (this.inlineActive) {
      const absent = new Set(this.debt?.items.filter((item) => item.absent).map((item) => item.key.slice(7)) ?? []);
      const survivors = this.inlineRoot.entries.filter((entry) => !absent.has(entry.digest));
      if (survivors.length + Number(!survivors.some((entry) => entry.digest === key.slice(7))) <= PAGE_CAPACITY) return true;
      await this.demoteInline();
      return false;
    }
    // A one-time promotion is bounded and paid; never scan descendants or
    // migrate on reads. Flush normalized debt before transferring ownership.
    await this.flushDebt();
    const rows = this.exec("SELECT key, size, peak_bytes, entries FROM blob_catalog LIMIT 2");
    if (rows.length > 1 || rows.length && (rows[0].key !== PAGE_PREFIX || rows[0].entries === null)) return false;
    const state = this.exec("SELECT generation, manifest FROM repository_state WHERE id = 1")[0];
    if (state && new TextEncoder().encode(state.manifest).length > INLINE_CAPABILITY.manifest_bytes) return false;
    let entries;
    try { entries = rows.length ? decodePage(rows[0]) : []; }
    catch (error) { this.catalogUncertain(); throw error; }
    // Legacy v1 can describe larger previously stored chunks. Its valid
    // public operations remain on the old representation rather than failing
    // a narrower optimization's size encoding after issuing a physical PUT.
    if (entries.some((entry) => entry.size > MAX_BLOB_BYTES)) return false;
    if (entries.length + Number(!entries.some((entry) => entry.digest === key.slice(7))) > PAGE_CAPACITY) return false;
    const acknowledgment = entries.appliedGeneration ?? this.debtContext.generation;
    if (acknowledgment > (state?.generation ?? 0)) {
      this.catalogUncertain();
      fail(409, "catalog acknowledgment exceeds authoritative generation");
    }
    const blob = encodeInline(entries, Math.max(this.peakBytes, rows[0]?.peak_bytes ?? 0), acknowledgment);
    if (!this.inlineSchema) {
      try { this.exec("ALTER TABLE repository_state ADD COLUMN catalog_root BLOB"); this.inlineSchema = true; }
      catch (error) { this.catalogUncertain(); throw error; }
    }
    if (state || rows.length) {
      try {
        this.ctx.storage.transactionSync(() => {
          this.exec("INSERT INTO repository_state (id, generation, manifest, catalog_root) VALUES (1, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET catalog_root = excluded.catalog_root",
            state?.generation ?? 0, state?.manifest ?? JSON.stringify({ generation: 0, refs: {}, packs: [] }), blob);
          if (rows.length) this.exec("DELETE FROM blob_catalog WHERE key = ?", PAGE_PREFIX);
        });
        await this.ctx.storage.sync();
      } catch (error) { this.catalogUncertain(); throw error; }
    }
    // A genuinely fresh repository creates its singleton only in the atomic
    // post-PUT transaction. An interruption still requires physical GC.
    this.inlineActive = true;
    this.inlineRoot = decodeInline(blob, state?.generation ?? 0);
    this.deferredPages = true;
    return true;
  }

  async putAndPublishInline(key, data, prefix, expected, next) {
    const { found, previous, folded } = await this.preparePut(key, prefix);
    try {
      await this.operation("r2_put", () => this.env.BLOBS.put(prefix + key, data), () => 0, data.byteLength, key);
      const bytes = count(this.liveBytes + data.byteLength - (previous?.size ?? 0));
      const peak = Math.max(this.peakBytes, bytes);
      const entries = found.entries.map((entry) => ({ ...entry }));
      const member = entries.find((entry) => entry.digest === key.slice(7));
      if (member) member.size = data.byteLength; else entries.push({ digest: key.slice(7), size: data.byteLength });
      entries.sort((left, right) => left.digest < right.digest ? -1 : left.digest > right.digest ? 1 : 0);
      // This acknowledges PRIOR resolved debt only. The new publication's
      // retirement has not happened and must remain recoverable after crash.
      const blob = encodeInline(entries, peak, this.debtContext.generation);
      return await this.operation("manifest_cas", async () => {
        let generation;
        const published = this.ctx.storage.transactionSync(() => {
          const state = this.exec("SELECT generation, manifest FROM repository_state WHERE id = 1")[0];
          generation = state?.generation ?? 0;
          const won = generation === expected;
          this.exec("INSERT INTO repository_state (id, generation, manifest, catalog_root) VALUES (1, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET generation = excluded.generation, manifest = excluded.manifest, catalog_root = excluded.catalog_root",
            won ? next.generation : generation,
            won ? JSON.stringify(next) : state?.manifest ?? JSON.stringify({ generation: 0, refs: {}, packs: [] }), blob);
          return won;
        });
        // Atomic coupling replaces the separate catalog-before-CAS barrier:
        // no durable winning manifest exists without its catalog/peak witness,
        // and a successful sync still precedes every retirement DELETE.
        await this.ctx.storage.sync();
        this.inlineRoot = decodeInline(blob, published ? next.generation : generation);
        this.liveObjects += previous ? 0 : 1;
        this.liveBytes = bytes; this.peakBytes = peak;
        this.peakOwner = { key: PAGE_PREFIX, peak };
        if (folded) this.debt = null;
        if (published) this.setDebtContext(next);
        return published;
      });
    } catch (error) { this.catalogUncertain(); throw error; }
  }

  catalogLookup(key) {
    try {
      if (this.inlineActive) {
        if (!this.inlineRoot) throw new Error("invalid inline owner requires physical reconciliation");
        const page = this.catalogRow(PAGE_PREFIX), entries = this.inlineRoot.entries.map((entry) => ({ ...entry }));
        entries.appliedGeneration = this.inlineRoot.appliedGeneration;
        const member = entries.find((entry) => entry.digest === key.slice(7));
        return { page, entries, member, size: member?.size };
      }
      const digest = key.slice(7);
      const keys = [key, ...ancestors(digest)];
      let legacy, page, entries, member;
      // Actual workerd cursors charge IN's temporary keys as read rows. Four
      // independent UNION ALL terms read only matching PK rows and fit the
      // existing compound-select limit; inspect every ancestor for overlaps.
      for (let offset = 0; offset < keys.length; offset += 4) {
        const batch = keys.slice(offset, offset + 4);
        const rows = this.exec(batch.map(() => "SELECT key, size, peak_bytes, entries FROM blob_catalog WHERE key = ?").join(" UNION ALL "), ...batch);
        for (const row of rows) {
          if (row.key === key) {
            if (row.entries !== null) throw new Error("invalid legacy catalog row");
            count(row.size); count(row.peak_bytes);
            legacy = row;
          } else {
            if (page) throw new Error("overlapping catalog pages");
            entries = decodePage(row);
            page = row;
            member = entries.find((entry) => entry.digest === digest);
          }
        }
      }
      if (legacy && member) throw new Error("duplicate legacy/page catalog membership");
      return { legacy, page, entries, member, size: legacy?.size ?? member?.size };
    } catch (error) { this.catalogUncertain(); throw error; }
  }

  missingPageKey(digest) {
    const target = PAGE_PREFIX + digest;
    const before = this.exec("SELECT key, size, peak_bytes, entries FROM blob_catalog WHERE key >= ? AND key < ? ORDER BY key DESC LIMIT 1", PAGE_PREFIX, target)[0];
    const after = this.exec("SELECT key, size, peak_bytes, entries FROM blob_catalog WHERE key > ? AND key < ? ORDER BY key LIMIT 1", target, PAGE_END)[0];
    if (!before && !after) return PAGE_PREFIX;
    let length = 0;
    for (const row of [before, after]) if (row) {
      decodePage(row);
      length = Math.max(length, commonPrefix(digest, row.key.slice(PAGE_PREFIX.length)) + 1);
    }
    if (length > 64) throw new Error("invalid catalog gap route");
    return PAGE_PREFIX + digest.slice(0, length);
  }

  writePage(key, entries, peak, appliedGeneration = null) {
    if (this.inlineActive) {
      if (key !== PAGE_PREFIX || appliedGeneration === null) throw new Error("invalid inline owner write");
      this.writeInline(entries, peak, appliedGeneration);
      return;
    }
    const row = encodePage(key, entries, peak, appliedGeneration);
    this.exec("INSERT INTO blob_catalog (key, size, peak_bytes, entries) VALUES (?, ?, ?, ?) ON CONFLICT(key) DO UPDATE SET size = excluded.size, peak_bytes = MAX(blob_catalog.peak_bytes, excluded.peak_bytes), entries = excluded.entries", row.key, row.size, row.peak_bytes, row.entries);
    if (appliedGeneration !== null) this.deferredPages = true;
  }

  putCatalogPage(key, size, found, peak, appliedGeneration = found.entries?.appliedGeneration ?? null) {
    if (found.legacy) {
      this.exec("INSERT INTO blob_catalog (key, size, peak_bytes) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET size = excluded.size, peak_bytes = MAX(blob_catalog.peak_bytes, excluded.peak_bytes)", key, size, peak);
      return key;
    }
    const digest = key.slice(7);
    const pageKey = found.page?.key ?? this.missingPageKey(digest);
    const entries = found.entries ?? [];
    const previous = entries.find((entry) => entry.digest === digest);
    if (previous) previous.size = size;
    else entries.push({ digest, size });
    entries.sort((left, right) => left.digest < right.digest ? -1 : left.digest > right.digest ? 1 : 0);
    peak = Math.max(peak, found.page?.peak_bytes ?? 0);
    if (entries.length <= PAGE_CAPACITY) { this.writePage(pageKey, entries, peak, appliedGeneration); return pageKey; }
    const groups = splitPage(entries);
    this.ctx.storage.transactionSync(() => {
      this.exec("DELETE FROM blob_catalog WHERE key = ?", pageKey);
      for (const group of groups) this.writePage(group.key, group.entries, peak, appliedGeneration);
    });
    return groups[0].key;
  }

  async preservePeak(excludedKey) {
    if (this.inlineActive && this.inlineRoot?.peak >= this.peakBytes) return;
    if (this.fallbackPeak >= this.peakBytes) return;
    if (this.peakOwner?.key !== excludedKey && this.peakOwner?.peak >= this.peakBytes) return;
    const witness = this.exec("SELECT key, peak_bytes FROM blob_catalog WHERE key <> ? AND peak_bytes >= ? LIMIT 1", excludedKey, this.peakBytes)[0];
    if (witness) { this.peakOwner = { key: witness.key, peak: witness.peak_bytes }; return; }
    await this.checkpointPeak(this.peakBytes);
  }

  async checkpointPeak(peak) {
    if (this.fallbackValid && this.fallbackPeak >= peak) return;
    this.exec(this.fallbackValid
      ? "INSERT INTO high_water (id, bytes) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET bytes = MAX(high_water.bytes, excluded.bytes)"
      : "INSERT INTO high_water (id, bytes) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET bytes = excluded.bytes", peak);
    await this.ctx.storage.sync();
    this.fallbackPeak = peak;
    this.fallbackValid = true;
  }

  async remove(key, prefix, knownSize, reconciling = false) {
    if (this.catalogPages && !reconciling) {
      await this.resolveDebt(prefix);
      await this.flushDebt();
      const receipt = await this.retirePages([key], prefix, 0);
      if (!receipt.outcomes[0].deleted) throw new Error("R2 deletion failed; inventory reconciliation required");
      return;
    }
    if (this.catalogPeak && !reconciling) this.requireCatalogCertain();
    const catalog = this.exec(this.catalogPeak ? "SELECT size, peak_bytes FROM blob_catalog WHERE key = ?" : "SELECT size FROM blob_catalog WHERE key = ?", key)[0];
    let size = knownSize ?? catalog?.size;
    if (size === undefined) {
      const object = await this.operation("r2_head", () => this.env.BLOBS.head(prefix + key), () => 0, 0, key);
      size = object?.size;
      if (this.catalogPeak && object && !reconciling) {
        this.catalogUncertain();
        fail(409, "uncataloged physical object requires inventory reconciliation");
      }
    }
    try {
      if (this.catalogPeak) await this.preservePeak(key);
      await this.operation("r2_delete", () => this.env.BLOBS.delete(prefix + key), () => 0, 0, key);
      this.exec("DELETE FROM blob_catalog WHERE key = ?", key);
      if (this.catalogPeak && this.peakOwner?.key === key) this.peakOwner = null;
      if (size !== undefined) {
        this.liveObjects = Math.max(0, this.liveObjects - 1);
        this.liveBytes = Math.max(0, this.liveBytes - size);
      }
    } catch (error) {
      if (this.catalogPeak) this.catalogUncertain();
      throw error;
    }
  }

  async preparePut(key, prefix) {
    if (this.catalogPeak) this.requireCatalogCertain();
    await this.resolveDebt(prefix);
    let found = this.catalogPages ? this.catalogLookup(key) : null;
    if (this.deferAllowed && found?.page && found.entries.appliedGeneration === null && !this.debt
        && this.debtContext.keys.length) {
      // A v1 page has no durable replay acknowledgment. Even after an older
      // client mutated this instance, its first conversion cannot trust a
      // surviving registration or an old all-success proof as absence evidence.
      try { this.loadDebtContext(); } catch (error) { this.catalogUncertain(); throw error; }
      await this.resolveDebt(prefix);
    }
    let folded = false;
    if (this.debt) {
      // A resurrected debt key must obtain its durable acknowledgment BEFORE
      // R2 PUT. Otherwise a post-PUT catalog failure leaves an old proof usable.
      const samePage = found.page && this.debt.items.every((item) => item.rowKey === found.page.key);
      const absent = new Set(this.debt.items.filter((item) => item.absent).map((item) => item.key.slice(7)));
      const survivors = samePage ? found.entries.filter((entry) => !absent.has(entry.digest)) : [];
      const projected = survivors.length + Number(!survivors.some((entry) => entry.digest === key.slice(7)));
      if (!this.debt.keys.includes(key) && samePage && survivors.length && projected <= PAGE_CAPACITY
          && found.entries.appliedGeneration !== null) {
        found.entries = survivors;
        found.member = survivors.find((entry) => entry.digest === key.slice(7));
        found.size = found.member?.size;
        folded = true;
      } else {
        await this.flushDebt();
        found = this.catalogLookup(key);
      }
    }
    const previous = found ? (found.size === undefined ? null : { size: found.size }) : this.exec("SELECT size FROM blob_catalog WHERE key = ?", key)[0];
    return { found, previous, folded };
  }

  async put(key, data, prefix) {
    if (this.inlineActive) {
      await this.resolveDebt(prefix);
      const found = this.catalogLookup(key);
      const absent = new Set(this.debt?.items.filter((item) => item.absent).map((item) => item.key.slice(7)) ?? []);
      const survivors = found.entries.filter((entry) => !absent.has(entry.digest));
      const capacity = survivors.length + Number(!survivors.some((entry) => entry.digest === key.slice(7)));
      if (capacity > PAGE_CAPACITY) await this.demoteInline();
    }
    const { found, previous, folded } = await this.preparePut(key, prefix);
    if (this.catalogPeak) {
      try {
        await this.operation("r2_put", () => this.env.BLOBS.put(prefix + key, data), () => 0, data.byteLength, key);
        const nextLive = this.liveBytes + data.byteLength - (previous?.size ?? 0);
        if (!Number.isSafeInteger(nextLive) || nextLive < 0) throw new Error("catalog byte total exceeds safe integer bound");
        this.peakBytes = Math.max(this.peakBytes, nextLive);
        const acknowledgment = this.catalogPages && (this.deferAllowed || found.entries?.appliedGeneration !== null && found.entries?.appliedGeneration !== undefined)
          ? this.debtContext.generation : null;
        const owner = this.catalogPages ? this.putCatalogPage(key, data.byteLength, found, this.peakBytes, acknowledgment) : key;
        if (!this.catalogPages) this.exec("INSERT INTO blob_catalog (key, size, peak_bytes) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET size = excluded.size, peak_bytes = MAX(blob_catalog.peak_bytes, excluded.peak_bytes)", key, data.byteLength, this.peakBytes);
        await this.ctx.storage.sync();
        this.liveObjects += previous ? 0 : 1;
        this.liveBytes = nextLive;
        this.peakOwner = { key: owner, peak: this.peakBytes };
        if (folded) this.debt = null;
        return;
      } catch (error) {
        this.catalogUncertain();
        throw error;
      }
    }
    await this.operation("r2_put", () => this.env.BLOBS.put(prefix + key, data), () => 0, data.byteLength, key);
    this.exec("INSERT INTO blob_catalog (key, size) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET size = excluded.size", key, data.byteLength);
    if (!previous) this.liveObjects++;
    this.liveBytes += data.byteLength - (previous?.size ?? 0);
    this.updatePeak();
    // The upload/catalog/high-water barrier always precedes CAS.
    await this.ctx.storage.sync();
  }

  async retire(keys, prefix, generation) {
    if (this.catalogPages) return this.retirePages(keys, prefix, generation);
    const outcomes = [];
    for (const key of keys) {
      let deleted = false;
      try { await this.remove(key, prefix); deleted = true; } catch { /* Durable debt remains retryable. */ }
      outcomes.push({ key, deleted });
    }
    try { await this.ctx.storage.sync(); }
    catch (error) { if (this.catalogPeak) this.catalogUncertain(); throw error; }
    return { generation, outcomes };
  }

  async preserveBatchPeak(doomed) {
    if (this.inlineActive && this.inlineRoot?.peak >= this.peakBytes) return;
    if (this.fallbackPeak >= this.peakBytes) return;
    if (this.peakOwner?.peak >= this.peakBytes && !doomed.has(this.peakOwner.key)) return;
    // The 256-key debt bound exceeds workerd's SQL parameter limit. Walk at
    // most the doomed count plus one matching witness using two bindings;
    // excluded rows never witness each other and no map of all pages is kept.
    let after = "";
    for (let inspected = 0; inspected <= doomed.size; inspected++) {
      const witness = this.exec("SELECT key, peak_bytes FROM blob_catalog WHERE key > ? AND peak_bytes >= ? ORDER BY key LIMIT 1", after, this.peakBytes)[0];
      if (!witness) break;
      if (!doomed.has(witness.key)) { this.peakOwner = { key: witness.key, peak: count(witness.peak_bytes) }; return; }
      after = witness.key;
    }
    await this.checkpointPeak(this.peakBytes);
  }

  async retirePages(keys, prefix, generation) {
    this.requireCatalogCertain();
    const groups = new Map(), requested = [], outcomes = [];
    let failed = false;
    try {
      // Retain only bounded request metadata, never all touched page payloads.
      for (const key of keys) {
        const found = this.catalogLookup(key);
        const row = found.legacy ?? (found.member ? found.page : null);
        if (!row) {
          const object = await this.operation("r2_head", () => this.env.BLOBS.head(prefix + key), () => 0, 0, key);
          if (object) { this.catalogUncertain(); fail(409, "uncataloged physical object requires inventory reconciliation"); }
        }
        const item = { key, rowKey: row?.key, size: found.size, deleted: false };
        requested.push(item);
        if (row) {
          const group = groups.get(row.key) ?? { page: !found.legacy, members: found.legacy ? 1 : found.entries.length, appliedGeneration: found.entries?.appliedGeneration ?? null, requested: 0 };
          group.requested++;
          groups.set(row.key, group);
        }
      }
      const doomed = new Set([...groups].filter(([, group]) => group.members === group.requested).map(([key]) => key));
      // Exclude the complete union: doomed pages cannot witness one another.
      await this.preserveBatchPeak(doomed);
      for (const item of requested) {
        try {
          await this.operation("r2_delete", () => this.env.BLOBS.delete(prefix + item.key), () => 0, 0, item.key);
          item.deleted = true;
        } catch { failed = true; }
        outcomes.push({ key: item.key, deleted: item.deleted });
      }
      const only = groups.size === 1 ? groups.values().next().value : null;
      const deferred = !failed && generation > 0 && this.deferAllowed && !this.debtContext?.eager && only?.page
        && only.appliedGeneration !== null && doomed.size === 0;
      if (!deferred) this.ctx.storage.transactionSync(() => {
        for (const [rowKey, group] of groups) {
          const removed = requested.filter((item) => item.rowKey === rowKey && item.deleted);
          if (!removed.length) continue;
          if (!group.page) { this.exec("DELETE FROM blob_catalog WHERE key = ?", rowKey); continue; }
          const row = this.catalogRow(rowKey);
          if (!row) throw new Error("catalog page disappeared during retirement");
          const digests = new Set(removed.map((item) => item.key.slice(7)));
          const entries = this.decodeCatalogRow(row);
          const survivors = entries.filter((entry) => !digests.has(entry.digest));
          const acknowledgment = entries.appliedGeneration !== null || this.deferAllowed
            ? generation || entries.appliedGeneration || this.debtContext?.generation || 0 : null;
          if (survivors.length || this.inlineActive) this.writePage(rowKey, survivors, row.peak_bytes, acknowledgment);
          else this.exec("DELETE FROM blob_catalog WHERE key = ?", rowKey);
        }
      });
      await this.ctx.storage.sync();
      for (const item of requested) if (item.deleted && item.size !== undefined) {
        this.liveObjects--;
        this.liveBytes -= item.size;
      }
      if (deferred) {
        this.debt = { generation, keys: this.debtContext.keys,
          items: requested.filter((item) => item.rowKey !== undefined).map((item) => ({ ...item, absent: true, appliedGeneration: only.appliedGeneration })), resolved: true };
      }
      if (this.peakOwner && doomed.has(this.peakOwner.key)) this.peakOwner = null;
      if (failed) this.catalogUncertain();
      return { generation, outcomes };
    } catch (error) {
      // SQL/transaction/barrier ambiguity is an error, never a clean receipt
      // with fabricated false outcomes after successful physical deletion.
      this.catalogUncertain();
      throw error;
    }
  }

  rebuildPages(objects, prefix, appliedGeneration = null) {
    // GC already owns this authoritative inventory array. Sort that existing
    // array; each builder holds <=32 entries, recursion is <=64 hex digits.
    for (const object of objects) {
      if (!object.key.startsWith(prefix) || !BLOB.test(object.key.slice(prefix.length))) throw new Error("noncanonical retained physical catalog key");
    }
    objects.sort((left, right) => left.key < right.key ? -1 : left.key > right.key ? 1 : 0);
    const build = (start, end, pageKey) => {
      if (end - start <= PAGE_CAPACITY) {
        if (end > start) this.writePage(pageKey, objects.slice(start, end).map((object) => ({ digest: object.key.slice(prefix.length + 7), size: count(object.size) })), this.peakBytes, appliedGeneration);
        return;
      }
      const first = objects[start].key.slice(prefix.length + 7), last = objects[end - 1].key.slice(prefix.length + 7);
      const length = commonPrefix(first, last) + 1;
      if (length > 64) throw new Error("duplicate physical catalog member");
      let groupStart = start;
      while (groupStart < end) {
        const nextKey = PAGE_PREFIX + objects[groupStart].key.slice(prefix.length + 7, prefix.length + 7 + length);
        let groupEnd = groupStart + 1;
        while (groupEnd < end && PAGE_PREFIX + objects[groupEnd].key.slice(prefix.length + 7, prefix.length + 7 + length) === nextKey) groupEnd++;
        build(groupStart, groupEnd, nextKey);
        groupStart = groupEnd;
      }
    };
    build(0, objects.length, PAGE_PREFIX);
  }

  async publishManifest(expected, next) {
    if (this.inlineActive && new TextEncoder().encode(JSON.stringify(next)).length > INLINE_CAPABILITY.manifest_bytes) {
      const current = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0]?.generation ?? 0;
      if (current !== expected) return this.operation("manifest_cas", async () => { await this.ctx.storage.sync(); return false; });
      await this.demoteInline();
    }
    if (this.catalogPeak) this.requireCatalogCertain();
    if (this.catalogPages && (this.debt || !this.debtLoaded && (this.deferredPages || this.deferAllowed))) {
      // A CAS-only stale caller must not trigger retirement HEADs or catalog
      // persistence. This exceptional read is charged; ordinary post-PUT CAS
      // already has no pending debt and retains its original one-row check.
      const generation = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0]?.generation ?? 0;
      if (generation !== expected) return this.operation("manifest_cas", async () => {
        await this.ctx.storage.sync();
        return false;
      });
    }
    await this.resolveDebt(this.repoPrefix);
    // Includes no-PUT CAS and an independent subsequent retirement set. The
    // old durable marker cannot disappear before its survivor rows sync.
    await this.flushDebt();
    try { return await this.operation("manifest_cas", async () => {
      const published = this.ctx.storage.transactionSync(() => {
        const generation = this.exec("SELECT generation FROM repository_state WHERE id = 1")[0]?.generation ?? 0;
        if (generation !== expected) return false;
        this.exec("INSERT INTO repository_state (id, generation, manifest) VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET generation = excluded.generation, manifest = excluded.manifest",
          next.generation, JSON.stringify(next));
        return true;
      });
      await this.ctx.storage.sync();
      if (published && this.debtLoaded) this.setDebtContext(next);
      return published;
    }); } catch (error) { if (this.catalogPeak) this.catalogUncertain(); throw error; }
  }

  async readBulk(body, prefix) {
      if (!plainObject(body) || !Array.isArray(body.chunks)
          || body.chunks.length === 0 || body.chunks.length > MAX_BULK_KEYS) fail(400, "invalid bulk read shape");
      let payloadBytes = 0;
      const lengths = new Map();
      // Bound payload and framing separately before any binding I/O.
      for (const chunk of body.chunks) {
        if (!plainObject(chunk) || typeof chunk.key !== "string" || chunk.key.length !== 71 || !BLOB.test(chunk.key)
            || !Number.isSafeInteger(chunk.len) || chunk.len <= 0 || chunk.len > MAX_BULK_CHUNK_BYTES
            || (lengths.has(chunk.key) && lengths.get(chunk.key) !== chunk.len)) fail(400, "invalid bulk read chunk");
        lengths.set(chunk.key, chunk.len);
        payloadBytes += chunk.len;
        if (!Number.isSafeInteger(payloadBytes) || payloadBytes > MAX_BULK_PAYLOAD_BYTES) fail(413, "bulk read payload too large");
      }
      const wireBytes = BULK_HEADER_BYTES + BULK_LENGTH_BYTES * body.chunks.length + payloadBytes;
      if (!Number.isSafeInteger(wireBytes) || wireBytes > MAX_BULK_RESPONSE_BYTES) fail(413, "bulk read response too large");
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

  async handle(request) {
    this.currentPhase = phase(request);
    this.bump("do_requests");
    const repo = repoName(request.headers.get("x-hosting-repo"));
    const prefix = `${repo}/`;
    this.repoPrefix = prefix;
    this.deferAllowed = request.headers.get(DEFERRED_HEADER) === "1";
    this.inlineAllowed = request.headers.get(INLINE_HEADER) === "1";
    this.confirmation = request.headers.get(CONFIRM_HEADER);
    const path = new URL(request.url).pathname;
    if (path === "/_services/manifest/first-read" && request.method === "POST") {
      const body = strictJson(await bytes(request, MAX_MANIFEST_BYTES));
      if (!plainObject(body) || Object.keys(body).length !== 4 || body.version !== 1
          || !validCount(body.generation) || typeof body.manifest !== "string" || !/^[0-9a-f]{64}$/.test(body.manifest)) fail(400, "invalid authority read plan");
      validateFirstBatch(body.chunks); // Complete bounds before any binding I/O.
      const manifest = await this.operation("manifest_get", async () => {
        const row = this.exec("SELECT manifest FROM repository_state WHERE id = 1")[0];
        return row ? JSON.parse(row.manifest) : {generation:0,refs:{},packs:[]};
      });
      if (!validCount(manifest.generation)) fail(502, "invalid authoritative generation");
      const matched = manifest.generation === body.generation;
      if (matched && await manifestFingerprint(manifest) !== body.manifest) fail(409, "same-generation manifest changed");
      const response = matched ? await this.readBulk(body, prefix) : new Response(null);
      response.headers.set("x-git-zero-internal-first-generation", String(manifest.generation));
      response.headers.set("x-git-zero-internal-first-matched", matched ? "1" : "0");
      return response;
    }
    if (path === "/_services/blobs/read" && request.method === "POST") {
      return this.readBulk(await parseJson(request, MAX_MANIFEST_BYTES), prefix);
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
        this.enableCatalogPeak(request);
        await this.put(key, data, prefix);
        return new Response(null, { status: 204 });
      }
      if (request.method === "DELETE") {
        this.enableCatalogPeak(request);
        await this.remove(key, prefix);
        try { if (!this.catalogPages) await this.ctx.storage.sync(); }
        catch (error) { if (this.catalogPeak) this.catalogUncertain(); throw error; }
        return new Response(null, { status: 204 });
      }
      return json({ error: "method not allowed" }, 405);
    }
    if (path === "/_services/manifest/upload-retire" && request.method === "POST") {
      const { body, keys, data } = await uploadFrame(request);
      this.enableCatalogPeak(request);
      const inline = await this.prepareInline(body.next, body.chunk.key);
      let published;
      if (inline) published = await this.putAndPublishInline(body.chunk.key, data, prefix, body.expected, body.next);
      else {
        await this.put(body.chunk.key, data, prefix);
        published = await this.publishManifest(body.expected, body.next);
      }
      // A stale upload is a durable orphan; a winning manifest may share it.
      // Only generation-checked recovery may decide whether to reclaim it.
      const retirement = published ? await this.retire(keys, prefix, body.next.generation) : null;
      return json({ version: 1, uploaded: body.chunk, published, retirement });
    }
    if (path === "/_services/manifest/retire" && request.method === "POST") {
      const body = await parseJson(request, MAX_MANIFEST_BYTES);
      const keys = retirementKeys(body);
      const published = await this.publishManifest(body.expected, body.next);
      if (!published) return json({ version: 1, published: false, retirement: null });
      if (keys.length) this.enableCatalogPeak(request);
      // CAS success is complete before retirement starts. Real failures keep
      // their accounting and durable debt, without relabeling successful CAS.
      return json({ version: 1, published: true,
        retirement: await this.retire(keys, prefix, body.next.generation) });
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
        if (!validManifestCas(body)) fail(400, "invalid manifest generation or shape");
        return json({ published: await this.publishManifest(body.expected, body.next) });
      }
      return json({ error: "method not allowed" }, 405);
    }
    if (path === "/_services/gc" && request.method === "POST") {
      const body = await parseJson(request, MAX_MANIFEST_BYTES);
      if (!Array.isArray(body.live_keys) || body.live_keys.some((key) => typeof key !== "string" || !BLOB.test(key))) fail(400, "invalid GC live keys");
      const current = this.exec("SELECT generation, manifest FROM repository_state WHERE id = 1")[0];
      const generation = current?.generation ?? 0;
      if (!Number.isSafeInteger(body.expected_generation) || body.expected_generation < 0) fail(400, "GC requires expected_generation");
      if (body.expected_generation !== generation) fail(409, "stale GC generation");
      // A stale/invalid recovery request never migrates. Recovery itself is
      // allowed while dirty; ordinary mutation remains blocked until its sync.
      if (this.catalogPeak || !this.catalogCertain) this.reloadCatalog();
      this.enableCatalogPeak(request, true);
      const live = new Set(body.live_keys);
      const objects = await this.inventory(prefix);
      const physicalBytes = objects.reduce((sum, object) => sum + object.size, 0);
      if (!Number.isSafeInteger(physicalBytes)) throw new Error("inventory byte total exceeds safe integer bound");
      if (this.catalogPeak) {
        this.peakBytes = Math.max(this.peakBytes, physicalBytes);
        try {
          // Inventory includes every orphan before deletion. A fallback is the
          // durable witness while the subsequent catalog reset is incomplete.
          await this.checkpointPeak(this.peakBytes);
          // Also stabilize a previously uncertain visible fallback, even when
          // it already covers H. SQL visibility alone is not a sync barrier.
          if (!this.catalogCertain) await this.ctx.storage.sync();
        } catch (error) { this.catalogUncertain(); throw error; }
      }
      let removed = 0;
      try {
        for (const object of objects) {
          const key = object.key.slice(prefix.length);
          if (!live.has(key)) {
            // Physical orphan names are untrusted. In particular an R2 key
            // named pages/foo must never address the SQL private page row.
            if (this.catalogPages) await this.operation("r2_delete", () => this.env.BLOBS.delete(object.key), () => 0, 0, key);
            else await this.remove(key, prefix, object.size, true);
            removed++;
          }
        }
        const retained = objects.filter((object) => live.has(object.key.slice(prefix.length)));
        // Reconcile catalog state after interrupted put/catalog updates as well.
        this.ctx.storage.transactionSync(() => {
          this.exec("DELETE FROM blob_catalog");
          if (this.inlineActive && retained.length <= PAGE_CAPACITY && new TextEncoder().encode(current?.manifest ?? "").length <= INLINE_CAPABILITY.manifest_bytes) {
            this.writeInline(retained.map((object) => ({ digest: object.key.slice(prefix.length + 7), size: count(object.size) })).sort((a, b) => a.digest < b.digest ? -1 : 1), this.peakBytes, generation);
          } else {
            if (this.inlineActive) { this.exec("UPDATE repository_state SET catalog_root = NULL WHERE id = 1"); this.inlineActive = false; this.inlineRoot = null; }
          if (this.catalogPages) this.rebuildPages(retained, prefix, this.deferredPages || this.deferAllowed ? generation : null);
          else for (const object of objects) {
            const key = object.key.slice(prefix.length);
            if (live.has(key)) {
              if (this.catalogPeak) this.exec("INSERT INTO blob_catalog (key, size, peak_bytes) VALUES (?, ?, ?)", key, object.size, this.peakBytes);
              else this.exec("INSERT INTO blob_catalog (key, size) VALUES (?, ?)", key, object.size);
            }
          }
          }
        });
        const retainedBytes = retained.reduce((sum, object) => sum + object.size, 0);
        if (!this.catalogPeak) { this.liveBytes = retainedBytes; this.updatePeak(); }
        await this.ctx.storage.sync();
        this.liveObjects = retained.length;
        this.liveBytes = retainedBytes;
        this.peakOwner = this.catalogPeak && retained.length && !this.catalogPages ? { key: retained[0].key.slice(prefix.length), peak: this.peakBytes } : null;
        this.catalogCertain = true;
        this.debt = null;
        this.setDebtContext(current ? JSON.parse(current.manifest) : { generation: 0, refs: {}, packs: [] });
      } catch (error) { if (this.catalogPeak) this.catalogUncertain(); throw error; }
      return json({ deleted: removed });
    }
    if (path === "/_services/metrics" && request.method === "GET") {
      const objects = await this.inventory(prefix);
      const liveBytes = objects.reduce((sum, object) => sum + object.size, 0);
      this.peakBytes = Math.max(this.peakBytes, liveBytes);
      // Observer LIST never reconciles/certifies pending debt. A reopened
      // historical page may legitimately differ until a paid mutation resolves
      // that bounded state. Arbitrary uncertainty remains sticky regardless.
      if (this.catalogPeak && !this.debt && (objects.length !== this.liveObjects || liveBytes !== this.liveBytes)) this.catalogUncertain();
      const row = this.exec("SELECT generation, LENGTH(CAST(manifest AS BLOB)) AS bytes FROM repository_state WHERE id = 1")[0];
      const counters = { ...this.totals };
      if (!this.sqlRowsKnown) { counters.do_rows_read = null; counters.do_rows_written = null; }
      return json({
        ...counters, r2_live_objects: objects.length, r2_live_bytes: liveBytes,
        r2_peak_bytes: this.peakBytes, do_sqlite_bytes: this.ctx.storage.sql.databaseSize,
        manifest_bytes: row?.bytes ?? 0, manifest_generation: row?.generation ?? 0,
        worker_cpu_ms: null, do_gb_seconds: null,
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
