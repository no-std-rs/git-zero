import { RepositoryCore } from "./host-core.mjs";
import { REPOSITORY, accessReady, authorize } from "./access.mjs";
import { CODEC_LIMITS } from "./codec.mjs";
import { READ_LIMITS, READ_HARD_LIMITS } from "./browse.mjs";
import { MEMORY_LIMITS, GATEWAY_MEMORY, ByteBudget, jobMemoryBytes } from "./resources.mjs";
import { refusal } from "./body.mjs";
import { apiRoute, hidden, json } from "./api.mjs";

function bounded(value, minimum, maximum, name) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) throw new TypeError(`invalid ${name}`);
  return value;
}
function limits(value, defaults, name, hard = defaults) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError(`invalid ${name}`);
  const result = { ...defaults };
  for (const [key, item] of Object.entries(value)) {
    if (!Object.hasOwn(defaults, key)) throw new TypeError(`unknown ${name}.${key}`);
    result[key] = bounded(item, 0, hard[key], `${name}.${key}`);
  }
  return Object.freeze(result);
}
function route(value, optional, name) {
  if (optional && (value === undefined || value === null)) return null;
  if (typeof value !== "string" || (value && !/^\/(?:[A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]+$/.test(value)) || (optional && !value)) throw new TypeError(`invalid ${name}`);
  return value;
}
const resolve = (value, env) => typeof value === "function" ? value(env) : value;
export { ByteBudget } from "./resources.mjs";
const enabled = (value) => value === true || value === "true";
// The factory owns explicit configuration; admission is per repository. No
// compiled asset, deployment binding or public route is imported implicitly.
export function createRepositoryHost(options) {
  if (!(options?.codec instanceof WebAssembly.Module)) throw new TypeError("codec must be a compiled WebAssembly.Module");
  options = { ...options, bindings: options.bindings ? Object.freeze({ ...options.bindings }) : null };
  const { bindings, authMode } = options;
  if (options.jurisdiction != null && options.jurisdiction !== "eu") throw new TypeError("jurisdiction must be null or eu");
  if (options.signingContext !== undefined && (typeof options.signingContext !== "string" || options.signingContext.length > 240)) throw new TypeError("invalid signingContext");
  const namespaceBinding = (env) => options.jurisdiction ? env[bindings.repositories]?.jurisdiction?.(options.jurisdiction) : env[bindings.repositories];
  if (!bindings || typeof bindings.repositories !== "string" || !bindings.repositories || (!options.bucket && (typeof bindings.objects !== "string" || !bindings.objects))) throw new TypeError("repository and object binding names are required");
  if (typeof authMode !== "function" && !["token", "gateway"].includes(authMode)) throw new TypeError("authMode must be supplied");
  for (const [key, value] of Object.entries(bindings)) if (!["repositories", "objects", "token", "grantKey"].includes(key) || typeof value !== "string" || !value) throw new TypeError("invalid binding name");
  if (typeof options.keyPrefix !== "string" || options.keyPrefix.length > 240 || (options.keyPrefix && (!/^[A-Za-z0-9_/-]+\/$/.test(options.keyPrefix) || options.keyPrefix.includes("//")))) throw new TypeError("keyPrefix must be an explicit empty string or safe slash-terminated prefix");
  const prefix = route(options.routePrefix, false, "routePrefix"), health = route(options.healthPath, true, "healthPath"), tests = route(options.testPrefix, true, "testPrefix");
  if (tests && (tests === prefix || (prefix && (tests.startsWith(prefix + "/") || prefix.startsWith(tests + "/"))))) throw new TypeError("testPrefix must be separate from routePrefix");
  const memory = limits(options.memory ?? {}, MEMORY_LIMITS, "memory");
  const gatewayMemory = limits(options.memory ?? {}, GATEWAY_MEMORY, "memory", MEMORY_LIMITS);
  if (memory.chunkBytes < 65536) throw new TypeError("chunkBytes must be at least 65536");
  if (!memory.refPageRows) throw new TypeError("refPageRows must be positive");
  const codecLimits = limits(options.codecLimits ?? {}, CODEC_LIMITS, "codecLimits");
  const readLimits = limits(options.readLimits ?? {}, READ_LIMITS, "readLimits", READ_HARD_LIMITS);
  for (const key of ["visibleObjects", "edges", "indexSteps", "refs", "pageSize", "jsonBytes"]) if (!readLimits[key]) throw new TypeError(`readLimits.${key} must be positive`);
  if (Object.hasOwn(options, "maxActiveJobs") || Object.hasOwn(options, "maxPendingJobs")) throw new TypeError("isolate-wide job-count admission was removed; use maxRequestsPerRepository");
  const concurrent = bounded(options.maxConcurrentJobs ?? 32, 1, 1024, "maxConcurrentJobs");
  const maxPending = options.maxPendingRequests === undefined ? null : bounded(options.maxPendingRequests, 0, 65536, "maxPendingRequests");
  const admissionMs = options.admissionWaitMs === undefined ? null : bounded(options.admissionWaitMs, 1, 600000, "admissionWaitMs");
  const maxRequests = options.maxRequestsPerRepository === undefined ? null : bounded(options.maxRequestsPerRepository, 1, 65536, "maxRequestsPerRepository");
  const idleMs = options.responseIdleMs === undefined ? null : bounded(options.responseIdleMs, 1, 600000, "responseIdleMs");
  const totalMs = options.responseTotalMs === undefined ? null : bounded(options.responseTotalMs, 0, 3600000, "responseTotalMs");
  const drainBytes = bounded(options.rejectionDrainBytes ?? 64 * 1024 * 1024, 65536, 256 * 1024 * 1024, "rejectionDrainBytes");
  const drainMs = bounded(options.rejectionDrainMs ?? 30000, 1, 60000, "rejectionDrainMs");
  const minimumRate = options.responseMinimumBytesPerSecond === undefined ? null : bounded(options.responseMinimumBytesPerSecond, 0, 1024 * 1024, "responseMinimumBytesPerSecond");
  const rateWindow = bounded(options.responseThroughputWindowMs ?? 30000, 10, 600000, "responseThroughputWindowMs");
  const cacheBytes = bounded(options.packCacheBytes ?? 512 * 1024 * 1024, 0, 8 * 1024 ** 3, "packCacheBytes");
  const cacheTotal = bounded(options.packCacheTotalBytes ?? 1024 ** 3, 0, 64 * 1024 ** 3, "packCacheTotalBytes");
  const cacheChunk = bounded(options.packCacheChunkBytes ?? 256 * 1024, 65536, 1024 * 1024, "packCacheChunkBytes");
  const cacheEntries = bounded(options.packCacheEntries ?? 8, 1, 256, "packCacheEntries");
  const cacheBuildMs = bounded(options.packCacheBuildMs ?? 120000, 1, 3600000, "packCacheBuildMs");
  const pinBytes = bounded(options.readPinBytes ?? 4 * 1024 * 1024, 0, 16 * 1024 * 1024, "readPinBytes");
  const pinEntries = bounded(options.readPinEntries ?? 4096, 0, 65536, "readPinEntries");
  const scratchBytes = options.readScratchBytes === undefined ? null : bounded(options.readScratchBytes, 0, CODEC_LIMITS.maxTemporaryBytes, "readScratchBytes");
  const repoScratch = options.readScratchRepositoryBytes === undefined ? null : bounded(options.readScratchRepositoryBytes, 0, CODEC_LIMITS.maxTemporaryBytes, "readScratchRepositoryBytes");
  const prepareMs = options.fetchPrepareMs === undefined ? null : bounded(options.fetchPrepareMs, 1, 3600000, "fetchPrepareMs");
  const bodyMs = options.requestBodyTotalMs === undefined ? null : bounded(options.requestBodyTotalMs, 1, 3600000, "requestBodyTotalMs");
  if (options.wantPolicy !== undefined && !["reachable", "catalog"].includes(options.wantPolicy)) throw new TypeError("wantPolicy must be reachable or catalog");
  if (options.memoryBudget !== undefined && !(options.memoryBudget instanceof ByteBudget)) throw new TypeError("memoryBudget must be a ByteBudget");
  const memoryBytes = bounded(options.memoryBudget?.limit ?? options.isolateMemoryBytes ?? 96 * 1024 * 1024, 1, 112 * 1024 * 1024, "isolateMemoryBytes");
  const memoryWaitMs = bounded(options.memoryWaitMs ?? 15000, 1, 600000, "memoryWaitMs");
  const contentionMs = bounded(options.memoryContentionMs ?? 10000, 1, 600000, "memoryContentionMs");
  const byteBudget = options.memoryBudget ?? new ByteBudget(memoryBytes, memoryWaitMs, contentionMs);
  const metadataBytes = (m) => Math.ceil((6 * 1048576 + Math.max(codecLimits.maxControlBytes, m.refPageRows * 65536) + 2 * 1048576) / 1048576) * 1048576;
  const jsonBytes = Math.ceil(((readLimits.pageSize + 2) * 65536 + 3 * readLimits.jsonBytes) / 1048576) * 1048576;
  const gatewayJobBytes = jobMemoryBytes(gatewayMemory, codecLimits, readLimits, pinBytes, pinEntries);
  const tokenJobBytes = jobMemoryBytes(memory, codecLimits, readLimits, pinBytes, pinEntries);
  const legacyExtra = (m) => Math.max(0, 2 * 8 * 1024 * 1024 - 2 * m.chunkBytes - m.chunkCacheBytes);
  if ((typeof authMode === "function" || authMode === "gateway") && Math.max(gatewayJobBytes + legacyExtra(gatewayMemory), metadataBytes(gatewayMemory), jsonBytes) > memoryBytes) throw new TypeError("isolateMemoryBytes cannot fit one configured host job");
  const entryDrains = { active: 0 };
  function environment(env) {
    const mode = resolve(authMode, env), policy = resolve(options.policy, env) ?? {};
    if ((options.isolateMemoryBytes !== undefined || options.memoryBudget !== undefined) && Math.max(mode === "gateway" ? gatewayJobBytes + legacyExtra(gatewayMemory) : tokenJobBytes + legacyExtra(memory), metadataBytes(mode === "gateway" ? gatewayMemory : memory), jsonBytes) > memoryBytes) throw new TypeError("isolateMemoryBytes cannot fit one configured host job");
    const internal = { WANT_POLICY: options.wantPolicy ?? (mode === "gateway" ? "reachable" : "catalog"), AUTH_MODE: mode, HOST_TOKEN: bindings.token ? env[bindings.token] : undefined, GRANT_KEY: bindings.grantKey ? env[bindings.grantKey] : undefined,
      RESPONSE_MINIMUM_RATE: minimumRate ?? (mode === "gateway" ? 16384 : 0), RESPONSE_RATE_WINDOW_MS: rateWindow,
      READ_PIN_BYTES: pinBytes, READ_PIN_ENTRIES: pinEntries, READ_SCRATCH_BYTES: scratchBytes ?? (mode === "gateway" ? 32 * 1024 * 1024 : codecLimits.maxTemporaryBytes), READ_SCRATCH_REPOSITORY_BYTES: repoScratch ?? (mode === "gateway" ? 64 * 1024 * 1024 : Infinity), FETCH_PREPARE_MS: prepareMs ?? (mode === "gateway" ? 30000 : 0),
      BYTE_BUDGET: byteBudget, JOB_MEMORY_BYTES: mode === "gateway" ? gatewayJobBytes : tokenJobBytes,
      PACK_CACHE_ENABLED: options.packCacheEnabled === undefined ? mode === "gateway" : enabled(resolve(options.packCacheEnabled, env)), PACK_CACHE_BYTES: cacheBytes, PACK_CACHE_TOTAL_BYTES: cacheTotal, PACK_CACHE_CHUNK_BYTES: cacheChunk, PACK_CACHE_ENTRIES: cacheEntries, PACK_CACHE_BUILD_MS: cacheBuildMs, MEMORY_WAIT_MS: memoryWaitMs,
      METADATA_MEMORY_BYTES: metadataBytes(mode === "gateway" ? gatewayMemory : memory), JSON_MEMORY_BYTES: jsonBytes, CACHE_READER_MEMORY_BYTES: 4 * cacheChunk,
      MAX_CONCURRENT: Math.min(concurrent, maxRequests ?? Infinity), MAX_PENDING: maxPending ?? (mode === "gateway" ? 64 : Infinity), ADMISSION_WAIT_MS: admissionMs ?? (mode === "gateway" ? 15000 : 0),
      BODY_TOTAL_MS: bodyMs ?? (mode === "gateway" ? 120000 : 0),
      SIGNING_CONTEXT: options.signingContext ?? "", KEY_PREFIX: options.keyPrefix, TEST_MODE: tests && enabled(resolve(Object.hasOwn(options, "testEnabled") ? options.testEnabled : true, env)) ? "true" : "false",
      IMPLICIT_CREATE: enabled(resolve(Object.hasOwn(options, "implicitCreate") ? options.implicitCreate : true, env)) ? "true" : "false", REPO_FORMAT: resolve(options.objectFormat ?? 0, env),
      REJECTION_DRAIN_BYTES: drainBytes, REJECTION_DRAIN_MS: drainMs, MEMORY: mode === "gateway" ? gatewayMemory : memory, MAX_REQUESTS: maxRequests ?? Infinity,
      RESPONSE_IDLE_MS: idleMs ?? (mode === "gateway" ? 10000 : 60000), RESPONSE_TOTAL_MS: totalMs ?? 0, CODEC_LIMITS: codecLimits, READ_LIMITS: readLimits,
      ALLOW_NON_FAST_FORWARD: String(policy.allowNonFastForward ?? false), MAX_STORED_BYTES: policy.maxStoredBytes == null ? undefined : String(policy.maxStoredBytes), MAX_PUSH_BYTES: policy.maxPushBytes == null ? undefined : String(policy.maxPushBytes) };
    Object.defineProperty(internal, "BLOBS", { get: () => options.bucket ? options.bucket(env) : env[bindings.objects] });
    return internal;
  }
  function canonical(request, env, direct) {
    const url = new URL(request.url); let path, isTest = false;
    if (tests && enabled(resolve(Object.hasOwn(options, "testEnabled") ? options.testEnabled : true, env)) && url.pathname.startsWith(tests + "/")) { path = "/_test/" + url.pathname.slice(tests.length + 1); isTest = true; }
    else if (!prefix || url.pathname.startsWith(prefix + "/")) path = url.pathname.slice(prefix.length);
    else return null;
    const parts = path.split("/"), namespace = isTest ? parts[2] : parts[1]?.endsWith(".git") ? parts[1].slice(0, -4) : null;
    if (!REPOSITORY.test(namespace ?? "")) return null;
    url.pathname = path;
    const headers = new Headers(request.headers);
    if (direct && headers.get("x-git-zero-namespace") !== namespace) return null;
    headers.set("x-git-zero-namespace", namespace);
    // Entry authorization needs only routing/headers. Do not consume/transfer
    // its body while constructing that view; the stub receives it exactly once.
    if (!isTest && parts.length > 4 && !apiRoute(new Request(url, { method: request.method, headers }))) return null;
    const normalized = direct ? new Request(url, new Request(request, { headers })) : new Request(url, { method: request.method, headers });
    return { request: normalized, namespace };
  }
  class Repository extends RepositoryCore {
    constructor(ctx, env) { super(ctx, environment(env), options.codec); this.bindingsEnv = env; }
    fetch(request) {
      const target = canonical(request, this.bindingsEnv, true);
      if (!target) return refusal(request, hidden(), this.env);
      if (this.ctx.id && namespaceBinding(this.bindingsEnv)?.idFromName(target.namespace).toString() !== this.ctx.id.toString()) return refusal(target.request, hidden(), this.env);
      return super.fetch(target.request);
    }
  }
  async function fetch(request, env) {
    const internal = environment(env), url = new URL(request.url);
    internal.DRAIN_STATE = entryDrains;
    if (health && url.pathname === health) { const ready = accessReady(internal); return json({ ready, runtime: "workerd", codec: "no_std Wasm", durableStorage: "R2 and SQLite", gitReceive: ready, gitFetch: ready }, ready ? 200 : 503); }
    const target = canonical(request, env, false), access = await authorize(target?.request ?? request, internal, target?.namespace ?? null);
    if (access instanceof Response) return refusal(request, access, internal, access.status === 200);
    if (!target) return refusal(request, hidden(), internal);
    const namespace = namespaceBinding(env);
    if (!namespace) return refusal(request, hidden(), internal);
    return namespace.get(namespace.idFromName(target.namespace)).fetch(new Request(request, { headers: target.request.headers }));
  }
  return Object.freeze({ fetch, Repository });
}
