import { RepositoryCore } from "./host-core.mjs";
import { REPOSITORY, accessReady, authorize } from "./access.mjs";
import { CODEC_LIMITS } from "./codec.mjs";
import { READ_LIMITS } from "./browse.mjs";
import { MEMORY_LIMITS } from "./resources.mjs";
import { apiRoute, hidden, json } from "./api.mjs";

function bounded(value, minimum, maximum, name) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) throw new TypeError(`invalid ${name}`);
  return value;
}
function limits(value, defaults, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError(`invalid ${name}`);
  const result = { ...defaults };
  for (const [key, item] of Object.entries(value)) {
    if (!Object.hasOwn(defaults, key)) throw new TypeError(`unknown ${name}.${key}`);
    result[key] = bounded(item, 0, defaults[key], `${name}.${key}`);
  }
  return Object.freeze(result);
}
function route(value, optional, name) {
  if (optional && (value === undefined || value === null)) return null;
  if (typeof value !== "string" || (value && !/^\/(?:[A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]+$/.test(value)) || (optional && !value)) throw new TypeError(`invalid ${name}`);
  return value;
}
const resolve = (value, env) => typeof value === "function" ? value(env) : value;
function semaphore(maxActive, maxPending) {
  let active = 0, pending = 0; const waiting = [];
  return { reserve() {
    if (pending >= maxPending) return null;
    pending++;
    return { async acquire() {
      if (active < maxActive) active++; else await new Promise((ready) => waiting.push(ready));
      let released = false;
      return () => {
        if (released) return; released = true; pending--;
        const next = waiting.shift(); if (next) next(); else active--;
      };
    } };
  } };
}

// The factory owns configuration and an isolate-local active-job gate. No
// compiled asset, deployment binding or public route is imported implicitly.
export function createRepositoryHost(options) {
  if (!(options?.codec instanceof WebAssembly.Module)) throw new TypeError("codec must be a compiled WebAssembly.Module");
  options = { ...options, bindings: options.bindings ? Object.freeze({ ...options.bindings }) : null };
  const { bindings, authMode } = options;
  if (!bindings || typeof bindings.repositories !== "string" || !bindings.repositories || (!options.bucket && (typeof bindings.objects !== "string" || !bindings.objects))) throw new TypeError("repository and object binding names are required");
  if (typeof authMode !== "function" && !["token", "gateway"].includes(authMode)) throw new TypeError("authMode must be supplied");
  for (const [key, value] of Object.entries(bindings)) if (!["repositories", "objects", "token", "grantKey"].includes(key) || typeof value !== "string" || !value) throw new TypeError("invalid binding name");
  if (typeof options.keyPrefix !== "string" || options.keyPrefix.length > 240 || (options.keyPrefix && (!/^[A-Za-z0-9_/-]+\/$/.test(options.keyPrefix) || options.keyPrefix.includes("//")))) throw new TypeError("keyPrefix must be an explicit empty string or safe slash-terminated prefix");
  const prefix = route(options.routePrefix, false, "routePrefix"), health = route(options.healthPath, true, "healthPath"), tests = route(options.testPrefix, true, "testPrefix");
  if (tests && (tests === prefix || (prefix && (tests.startsWith(prefix + "/") || prefix.startsWith(tests + "/"))))) throw new TypeError("testPrefix must be separate from routePrefix");
  const memory = limits(options.memory ?? {}, MEMORY_LIMITS, "memory");
  if (memory.chunkBytes < 65536) throw new TypeError("chunkBytes must be at least 65536");
  if (!memory.refPageRows) throw new TypeError("refPageRows must be positive");
  const codecLimits = limits(options.codecLimits ?? {}, CODEC_LIMITS, "codecLimits");
  const readLimits = limits(options.readLimits ?? {}, READ_LIMITS, "readLimits");
  for (const key of ["visibleObjects", "edges", "refs", "pageSize", "jsonBytes"]) if (!readLimits[key]) throw new TypeError(`readLimits.${key} must be positive`);
  const maxActive = bounded(options.maxActiveJobs ?? 1, 1, 32, "maxActiveJobs");
  const maxPending = bounded(options.maxPendingJobs ?? 64, maxActive, 1024, "maxPendingJobs");
  const maxRequests = bounded(options.maxRequestsPerRepository ?? 32, 1, maxPending, "maxRequestsPerRepository");
  const gate = semaphore(maxActive, maxPending);
  function environment(env) {
    const mode = resolve(authMode, env), policy = resolve(options.policy, env) ?? {};
    const internal = { AUTH_MODE: mode, HOST_TOKEN: bindings.token ? env[bindings.token] : undefined, GRANT_KEY: bindings.grantKey ? env[bindings.grantKey] : undefined,
      KEY_PREFIX: options.keyPrefix, TEST_MODE: tests && resolve(options.testEnabled ?? true, env) ? "true" : "false",
      IMPLICIT_CREATE: resolve(options.implicitCreate ?? true, env) ? "true" : "false", REPO_FORMAT: resolve(options.objectFormat ?? 0, env),
      MEMORY: memory, CODEC_LIMITS: codecLimits, READ_LIMITS: readLimits,
      ALLOW_NON_FAST_FORWARD: String(policy.allowNonFastForward ?? false), MAX_STORED_BYTES: policy.maxStoredBytes == null ? undefined : String(policy.maxStoredBytes), MAX_PUSH_BYTES: policy.maxPushBytes == null ? undefined : String(policy.maxPushBytes) };
    Object.defineProperty(internal, "BLOBS", { get: () => options.bucket ? options.bucket(env) : env[bindings.objects] });
    return internal;
  }
  function canonical(request, env, direct) {
    const url = new URL(request.url); let path, isTest = false;
    if (tests && resolve(options.testEnabled ?? true, env) && url.pathname.startsWith(tests + "/")) { path = "/_test/" + url.pathname.slice(tests.length + 1); isTest = true; }
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
    constructor(ctx, env) { super(ctx, environment(env), options.codec, gate, maxRequests); this.bindingsEnv = env; }
    fetch(request) {
      const target = canonical(request, this.bindingsEnv, true);
      if (!target) return Promise.resolve(hidden());
      if (this.ctx.id && this.bindingsEnv[bindings.repositories]?.idFromName(target.namespace).toString() !== this.ctx.id.toString()) return Promise.resolve(hidden());
      return super.fetch(target.request);
    }
  }
  async function fetch(request, env) {
    const internal = environment(env), url = new URL(request.url);
    if (health && url.pathname === health) { const ready = accessReady(internal); return json({ ready, runtime: "workerd", codec: "no_std Wasm", durableStorage: "R2 and SQLite", gitReceive: ready, gitFetch: ready }, ready ? 200 : 503); }
    const target = canonical(request, env, false), access = await authorize(target?.request ?? request, internal, target?.namespace ?? null);
    if (access instanceof Response) return access;
    if (!target) return hidden();
    const namespace = env[bindings.repositories];
    return namespace.get(namespace.idFromName(target.namespace)).fetch(new Request(request, { headers: target.request.headers }));
  }
  return Object.freeze({ fetch, Repository });
}
