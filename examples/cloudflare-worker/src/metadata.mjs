import { SnapshotError } from "./snapshots.mjs";

function names(value) {
  const text = value ?? "";
  if (typeof text !== "string") throw new SnapshotError("Public selections must be comma-separated strings", 503);
  if (text.length > 4096) throw new SnapshotError("Public selection exceeds configuration limit", 503);
  const selected = text.split(",").map((name) => name.trim()).filter(Boolean);
  if (selected.length > 64) throw new SnapshotError("Public selection exceeds 64 names", 503);
  return new Set(selected);
}

function headers(request, env) {
  const result = Object.create(null);
  for (const name of names(env.EXPOSED_CF_HEADERS ?? "cf-ray")) {
    const lower = name.toLowerCase();
    if (!/^cf-[a-z0-9-]+$/.test(lower) || lower.startsWith("cf-access-") || lower === "cf-authorization") {
      throw new SnapshotError("Only public Cloudflare headers may be selected", 503);
    }
    const value = request.headers.get(lower);
    if (value !== null) result[lower] = value;
  }
  return result;
}

// Copy data properties into fresh JSON containers. Never invoke a binding's
// toJSON method/getters or traverse host objects such as KV/R2/service bindings.
export class JsonProjection {
  remaining = 1024;
  parents = new Set();

  copy(value, depth = 0) {
    if (--this.remaining < 0 || depth > 16) throw new SnapshotError("Metadata exceeds JSON projection limits", 413);
    if (value === null || typeof value === "string" || typeof value === "boolean") return value;
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value !== "object" || this.parents.has(value)) throw new TypeError("Not JSON data");
    const array = Array.isArray(value);
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== (array ? Array.prototype : Object.prototype) && prototype !== null) throw new TypeError("Not a plain JSON container");
    const keys = Object.keys(value);
    if (keys.length > this.remaining) throw new SnapshotError("Metadata exceeds JSON projection limits", 413);
    if (array && (keys.length !== value.length || keys.some((key, index) => key !== String(index)))) throw new TypeError("Not a JSON array");
    this.parents.add(value);
    const result = array ? [] : Object.create(null);
    for (const key of keys) {
      const property = Object.getOwnPropertyDescriptor(value, key);
      if (!property || !("value" in property)) throw new TypeError("Not a data property");
      result[key] = this.copy(property.value, depth + 1);
    }
    this.parents.delete(value);
    return result;
  }

  project(value, status) {
    try { return this.copy(value); }
    catch (error) {
      if (error instanceof SnapshotError) throw error;
      throw new SnapshotError("Public metadata must contain only plain JSON values", status);
    }
  }
}

export function captureMetadata(request, env) {
  const projection = new JsonProjection();
  const cf = projection.project(request.cf ?? {}, 400);
  const publicHeaders = headers(request, env);
  const vars = Object.create(null);
  // String vars and secrets are indistinguishable at runtime. The deployment
  // owner must explicitly select only public names; never enumerate env.
  for (const name of names(env.EXPOSED_VARS)) {
    const property = Object.getOwnPropertyDescriptor(env, name);
    if (!property) continue;
    if (!("value" in property)) throw new SnapshotError("Public bindings must be data properties", 503);
    const value = property.value;
    if (value !== undefined) vars[name] = projection.project(value, 503);
  }
  const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
  return [
    json(cf), json(publicHeaders), json(vars),
    json({ platform: "Cloudflare Workers", format: 1, history: "one root commit; no parents" }),
  ];
}
