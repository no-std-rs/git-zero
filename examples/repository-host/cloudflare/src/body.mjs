// HTTP content coding wraps the Git byte stream. The Rust consumers enforce
// decoded control/pack limits; this bound also caps compressed transport input.
import { RequestError } from "./errors.mjs";
const budgets = new WeakMap();
export function bindBodyBudget(request, env) { budgets.set(request, { totalMs: env.BODY_TOTAL_MS ?? 0, deadline: env.BODY_DEADLINE ?? (env.BODY_TOTAL_MS ? Date.now() + env.BODY_TOTAL_MS : Infinity), signal: env.BODY_SIGNAL }); }
export function bodyBudget(request) { return budgets.get(request) ?? {}; }
export function decodedBody(request, maximum, label) {
  const coding = request.headers.get("content-encoding")?.toLowerCase();
  if (!coding || coding === "identity") return request.body;
  if ((coding !== "gzip" && coding !== "x-gzip") || !request.body) throw new RequestError("unsupported HTTP content coding");
  let remaining = BigInt(maximum);
  return request.body.pipeThrough(new TransformStream({
    transform(chunk, controller) {
      remaining -= BigInt(chunk.byteLength);
      if (remaining < 0n) throw new RequestError(`compressed ${label} exceed byte limit`);
      controller.enqueue(chunk);
    },
  })).pipeThrough(new DecompressionStream("gzip"));
}

// The deadline covers waiting for input, not time spent processing a chunk.
export async function* requestBody(stream, idleMs = 60000, { totalMs = 0, deadline: absoluteDeadline = Infinity, preserve = false, signal } = {}) {
  if (!stream) return;
  const reader = stream.getReader(), deadline = Math.min(absoluteDeadline, totalMs ? Date.now() + totalMs : Infinity);
  let finished = false;
  const abort = () => { reader.cancel().catch(() => {}); };
  signal?.addEventListener("abort", abort, { once: true });
  try {
    for (;;) {
      if (signal?.aborted) throw new RequestError("request body cancelled");
      if (Date.now() >= deadline) throw new RequestError("request body total deadline exceeded");
      let timer, item;
      try {
        item = await Promise.race([
          reader.read(),
          new Promise((_, reject) => { timer = setTimeout(() => reject(new RequestError(Date.now() >= deadline ? "request body total deadline exceeded" : "request body idle")), Math.min(typeof idleMs === "function" ? idleMs() : idleMs, deadline - Date.now())); }),
        ]);
      } catch (error) {
        throw error instanceof RequestError ? error : new RequestError(String(error.message ?? error), { cause: error });
      } finally { clearTimeout(timer); }
      if (signal?.aborted) throw new RequestError("request body cancelled");
      if (item.done) { finished = true; return; }
      yield item.value;
    }
  } finally {
    // A transport's cancellation promise may itself wait for the stalled peer.
    // No abandoned read can resume the parser after this iterator has stopped.
    signal?.removeEventListener("abort", abort);
    if (!preserve || finished) reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

// Refusals consume a bounded transport tail so a still-uploading Git client can
// receive the status instead of observing a reset. Never parse unauthorized data.
export async function refusal(request, response, env = {}, authenticated = false) {
  if (!request.body || request.body.locked || request.bodyUsed) return response;
  const state = env.DRAIN_STATE ??= { active: 0 };
  if (state.active >= 2) { request.body.cancel().catch(() => {}); return response; }
  state.active++;
  const maximum = authenticated ? env.REJECTION_DRAIN_BYTES ?? 64 * 1024 * 1024 : Math.min(env.REJECTION_DRAIN_BYTES ?? Infinity, 1024 * 1024);
  const deadline = Date.now() + (authenticated ? env.REJECTION_DRAIN_MS ?? 30000 : Math.min(env.REJECTION_DRAIN_MS ?? Infinity, 5000));
  let discarded = 0;
  try {
    for await (const part of requestBody(request.body, () => Math.max(1, Math.min(10000, deadline - Date.now())))) {
      discarded += part.byteLength;
      if (discarded >= maximum || Date.now() >= deadline) break;
    }
  } catch { /* A vanished/over-bound peer cannot be made to read a refusal. */ }
  finally { state.active--; }
  return response;
}
