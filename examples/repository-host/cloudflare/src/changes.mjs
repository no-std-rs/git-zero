import { ApiError, encode, json } from "./api.mjs";
import { bytes } from "./storage.mjs";

const MAX_JSON = 512 * 1024;
function count(value, fallback, maximum = Number.MAX_SAFE_INTEGER) {
  if (value === null || value === undefined) return fallback;
  if (!/^(0|[1-9][0-9]*)$/.test(String(value)) || !Number.isSafeInteger(Number(value)) || Number(value) > maximum) throw new ApiError("invalid changes parameter");
  return Number(value);
}
const pack = (value) => btoa(JSON.stringify(value)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
function unpack(value) {
  if (value.length > 1024 || !/^[A-Za-z0-9_-]+$/.test(value)) throw new ApiError("invalid changes cursor");
  try {
    const result = JSON.parse(atob(value.replaceAll("-", "+").replaceAll("_", "/")));
    if (!result || Array.isArray(result) || typeof result !== "object" || pack(result) !== value) throw new Error();
    return result;
  } catch { throw new ApiError("invalid changes cursor"); }
}
function name(value) {
  const data = bytes(value), pieces = [];
  for (let at = 0; at < data.length; at += 4096) pieces.push(String.fromCharCode(...data.subarray(at, at + 4096)));
  let decoded = null;
  try { decoded = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(data); } catch { /* Exact bytes remain available. */ }
  return { name: decoded, name64: btoa(pieces.join("")).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "") };
}

// A cursor snapshots the log's high-water mark, not the current ref generation.
// Later pushes do not invalidate a consumer's partially delivered publication.
export function changes(store, request) {
  if (request.method !== "GET") return json({ error: "method not allowed" }, 405);
  const params = new URL(request.url).searchParams;
  const current = store.exec("SELECT generation FROM repository WHERE id=1")[0].generation;
  const floor = store.exec("SELECT generation FROM publication_start WHERE id=1")[0].generation;
  const cursor = params.has("cursor") ? unpack(params.get("cursor")) : null;
  const after = count(params.get("after"), cursor?.after ?? 0), limit = count(params.get("limit"), 20, 100);
  if (!limit) throw new ApiError("changes limit must be positive");
  if (after < floor) return json({ error: "push records unavailable before coverage boundary", recordedAfterGeneration: floor }, 409);
  if (after > current) throw new ApiError("after is beyond the current generation", 409);
  if (cursor && (cursor.v !== 1 || cursor.op !== "changes" || cursor.namespace !== store.namespace || cursor.after !== after)) throw new ApiError("invalid changes cursor");
  const through = count(cursor?.through, current), generation = count(cursor?.generation, after), seq = count(cursor?.seq, Number.MAX_SAFE_INTEGER);
  const completed = count(cursor?.completed, after);
  if (through > current || through < after || generation < after || generation > through || completed < after || completed > generation || (cursor && generation === after)) throw new ApiError("invalid changes cursor position");
  const rows = store.exec("SELECT u.*,p.time,p.actor64 FROM publication_updates u JOIN publications p ON p.generation=u.generation WHERE (u.generation,u.seq)>(?,?) AND u.generation<=? ORDER BY u.generation,u.seq LIMIT ?", generation, seq, through, limit + 1);
  const publications = []; let used = 512, taken = 0, last;
  for (const row of rows.slice(0, limit)) {
    const update = { seq: row.seq, ...name(row.name), oldId: row.old_oid, newId: row.new_oid };
    const actor = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(Uint8Array.from(atob(row.actor64.replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0)));
    const cost = encode(JSON.stringify(update)).length + encode(JSON.stringify(actor)).length + 128;
    if (used + cost > MAX_JSON) { if (!taken) throw new ApiError("push record exceeds JSON byte limit", 413); break; }
    let event = publications.at(-1);
    if (event?.generation !== row.generation) { event = { generation: row.generation, time: row.time, actor, updates: [], complete: true }; publications.push(event); }
    event.updates.push(update); used += cost; last = row; taken++;
  }
  const more = rows[taken];
  if (more && publications.length && more.generation === publications.at(-1).generation) publications.at(-1).complete = false;
  const completedThrough = more ? publications.filter((event) => event.complete).at(-1)?.generation ?? completed : through;
  const nextCursor = more ? pack({ v: 1, op: "changes", namespace: store.namespace, after, through, completed: completedThrough, generation: last.generation, seq: last.seq }) : null;
  return json({ recordedAfterGeneration: floor, throughGeneration: through, completedThrough, publications, nextCursor });
}
