import { ApiError, json, readJson } from "./api.mjs";
import { RequestError } from "./errors.mjs";

export class PolicyError extends RequestError {}
const fields = ["allowNonFastForward", "maxStoredBytes", "maxPushBytes"];
function cap(value, name) {
  if (value === null || value === undefined || value === "") return null;
  if (!/^(0|[1-9][0-9]*)$/.test(String(value)) || !Number.isSafeInteger(Number(value))) throw new Error(`invalid deployment ${name}`);
  return Number(value);
}
export function deploymentPolicy(env) {
  if (env.ALLOW_NON_FAST_FORWARD !== undefined && !["true", "false"].includes(env.ALLOW_NON_FAST_FORWARD)) throw new Error("invalid deployment ALLOW_NON_FAST_FORWARD");
  return { allowNonFastForward: env.ALLOW_NON_FAST_FORWARD === "true", maxStoredBytes: cap(env.MAX_STORED_BYTES, "MAX_STORED_BYTES"), maxPushBytes: cap(env.MAX_PUSH_BYTES, "MAX_PUSH_BYTES") };
}
const minimum = (left, right) => left === null ? right : right === null ? left : Math.min(left, right);
export function repositoryPolicy(store) {
  const defaults = deploymentPolicy(store.env), row = store.exec("SELECT * FROM host_policy WHERE id=1")[0];
  const overrides = { allowNonFastForward: row?.allow_non_ff === null || row?.allow_non_ff === undefined ? null : Boolean(row.allow_non_ff), maxStoredBytes: row?.max_stored ?? null, maxPushBytes: row?.max_push ?? null };
  return { policy: { allowNonFastForward: overrides.allowNonFastForward ?? defaults.allowNonFastForward, maxStoredBytes: minimum(overrides.maxStoredBytes, defaults.maxStoredBytes), maxPushBytes: minimum(overrides.maxPushBytes, defaults.maxPushBytes) }, overrides, deployment: defaults, storage: store.storageAccounting() };
}
export async function policyRoute(store, request) {
  if (!store.exec("SELECT 1 FROM repository WHERE id=1").length) return json({ error: "unknown route" }, 404);
  if (request.method === "POST") { store.advanceStorageAccounting(); return json(repositoryPolicy(store), store.storageAccounting().ready ? 200 : 202); }
  if (request.method === "GET") return json(repositoryPolicy(store));
  if (request.method !== "PATCH") return json({ error: "method not allowed" }, 405);
  const value = await readJson(request);
  if (Object.keys(value).some((key) => !fields.includes(key))) throw new ApiError("unknown policy field");
  for (const [key, item] of Object.entries(value)) {
    if (item === null) continue;
    if (key === "allowNonFastForward" ? typeof item !== "boolean" : typeof item !== "number" || !Number.isSafeInteger(item) || item < 0) throw new ApiError(`invalid ${key}`);
  }
  const prior = repositoryPolicy(store).overrides, next = { ...prior, ...value };
  store.exec("INSERT INTO host_policy(id,allow_non_ff,max_stored,max_push) VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET allow_non_ff=excluded.allow_non_ff,max_stored=excluded.max_stored,max_push=excluded.max_push", next.allowNonFastForward === null ? null : Number(next.allowNonFastForward), next.maxStoredBytes, next.maxPushBytes);
  return json(repositoryPolicy(store));
}
