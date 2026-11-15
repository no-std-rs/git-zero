import { RepositoryCore } from "./host-core.mjs";
import { REPOSITORY, accessReady, authorize } from "./access.mjs";
import { CODEC_LIMITS } from "./codec.mjs";
import { READ_LIMITS, READ_HARD_LIMITS } from "./browse.mjs";
import { MEMORY_LIMITS, GATEWAY_MEMORY } from "./resources.mjs";
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
const enabled = (value) => value === true || value === "true";
// The factory owns explicit configuration; admission is per repository. No
// compiled asset, deployment binding or public route is imported implicitly.
export function createRepositoryHost(options) {
  if (!(options?.codec instanceof WebAssembly.Module)) throw new TypeError("codec must be a compiled WebAssembly.Module");
  options = { ...options, bindings: options.bindings ? Object.freeze({ ...options.bindings }) : null };
  const { bindings, authMode } = options;
  if (options.jurisdiction != null && options.jurisdiction !== "eu") throw new TypeError("jurisdiction must be null or eu");
  const namespaceBinding = (env) => options.jurisdiction ? env[bindings.repositories].jurisdiction(options.jurisdiction) : env[bindings.repositories];
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
  if (Object.hasOwn(options, "maxActiveJobs") || Object.hasOwn(options, "maxPendingJobs")) throw new TypeError("isolate-wide admission was removed; use maxRequestsPerRepository");
  const maxRequests = options.maxRequestsPerRepository === undefined ? null : bounded(options.maxRequestsPerRepository, 1, 65536, "maxRequestsPerRepository");
  const idleMs = options.responseIdleMs === undefined ? null : bounded(options.responseIdleMs, 1, 600000, "responseIdleMs");
  const totalMs = options.responseTotalMs === undefined ? null : bounded(options.responseTotalMs, 0, 3600000, "responseTotalMs");
  function environment(env) {
    const mode = resolve(authMode, env), policy = resolve(options.policy, env) ?? {};
    const internal = { AUTH_MODE: mode, HOST_TOKEN: bindings.token ? env[bindings.token] : undefined, GRANT_KEY: bindings.grantKey ? env[bindings.grantKey] : undefined,
      KEY_PREFIX: options.keyPrefix, TEST_MODE: tests && enabled(resolve(Object.hasOwn(options, "testEnabled") ? options.testEnabled : true, env)) ? "true" : "false",
      IMPLICIT_CREATE: enabled(resolve(Object.hasOwn(options, "implicitCreate") ? options.implicitCreate : true, env)) ? "true" : "false", REPO_FORMAT: resolve(options.objectFormat ?? 0, env),
      MEMORY: mode === "gateway" ? gatewayMemory : memory, MAX_REQUESTS: maxRequests ?? (mode === "gateway" ? 32 : Infinity),
      RESPONSE_IDLE_MS: idleMs ?? (mode === "gateway" ? 10000 : 60000), RESPONSE_TOTAL_MS: totalMs ?? (mode === "gateway" ? 300000 : 0), CODEC_LIMITS: codecLimits, READ_LIMITS: readLimits,
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
    const normalized = direct ? new Request(url, new Request(request, { headers })) : new Request(url, { method: request.method, headers });
    if (!isTest && parts.length > 4 && !apiRoute(normalized)) return null;
    return { request: normalized, namespace };
  }
  class Repository extends RepositoryCore {
    constructor(ctx, env) { super(ctx, environment(env), options.codec); this.bindingsEnv = env; }
    fetch(request) {
      const target = canonical(request, this.bindingsEnv, true);
      if (!target) return Promise.resolve(hidden());
      if (this.ctx.id && namespaceBinding(this.bindingsEnv)?.idFromName(target.namespace).toString() !== this.ctx.id.toString()) return Promise.resolve(hidden());
      return super.fetch(target.request);
    }
  }
  async function fetch(request, env) {
    const internal = environment(env), url = new URL(request.url);
    if (health && url.pathname === health) { const ready = accessReady(internal); return json({ ready, runtime: "workerd", codec: "no_std Wasm", durableStorage: "R2 and SQLite", gitReceive: ready, gitFetch: ready }, ready ? 200 : 503); }
    const target = canonical(request, env, false), access = await authorize(target?.request ?? request, internal, target?.namespace ?? null);
    if (access instanceof Response) return access;
    if (!target) return hidden();
    const namespace = namespaceBinding(env);
    return namespace.get(namespace.idFromName(target.namespace)).fetch(new Request(request, { headers: target.request.headers }));
  }
  return Object.freeze({ fetch, Repository });
}
