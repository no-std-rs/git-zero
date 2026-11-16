// HTTP content coding wraps the Git byte stream. The Rust consumers enforce
// decoded control/pack limits; this bound also caps compressed transport input.
import { RequestError } from "./errors.mjs";
const budgets = new WeakMap();
export function bindBodyBudget(request, env) {
  budgets.set(request, { totalMs: env.BODY_TOTAL_MS ?? 0, signal: env.BODY_SIGNAL,
    phase: env.BODY_PHASE ?? { mode: "prefix", deadline: Infinity, rate: env.BODY_MINIMUM_RATE ?? 0, windowMs: env.BODY_RATE_WINDOW_MS ?? 30000 }, managed: env.BODY_MANAGED ?? false });
}
export function startPackBody(request) {
  const phase = budgets.get(request)?.phase;
  if (phase) { phase.mode = "pack"; phase.credit = phase.rate * phase.windowMs / 1000; }
}
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
export async function* requestBody(stream, idleMs = 60000, { totalMs = 0, deadline: absoluteDeadline = Infinity, preserve = false, signal, phase, managed = false } = {}) {
  if (!stream) return;
  const reader = stream.getReader(), deadline = Math.min(absoluteDeadline, totalMs ? Date.now() + totalMs : Infinity);
  if (phase && !managed && phase.mode === "prefix" && phase.deadline === Infinity && totalMs) phase.deadline = Date.now() + totalMs;
  const remainingTime = () => {
    if (!phase) return deadline - Date.now();
    if (managed) return Infinity;
    if (phase.mode === "prefix") return phase.deadline - Date.now();
    return phase.rate ? phase.credit / phase.rate * 1000 : Infinity;
  };
  const failure = () => phase?.mode === "pack" ? "request body minimum throughput exceeded" : "request body total deadline exceeded";
  let finished = false;
  const abort = () => { reader.cancel().catch(() => {}); };
  signal?.addEventListener("abort", abort, { once: true });
  try {
    for (;;) {
      if (signal?.aborted) throw new RequestError("request body cancelled");
      if (remainingTime() <= 0) throw new RequestError(failure());
      let timer, item;
      const remaining = remainingTime(), began = Date.now(), idle = typeof idleMs === "function" ? idleMs() : idleMs;
      const totalFirst = remaining <= idle;
      try {
        item = await Promise.race([
          reader.read(),
          new Promise((_, reject) => { timer = setTimeout(() => reject(new RequestError(totalFirst ? failure() : "request body idle")), Math.max(0, Math.min(idle, remaining))); }),
        ]);
      } catch (error) {
        throw error instanceof RequestError ? error : new RequestError(String(error.message ?? error), { cause: error });
      } finally { clearTimeout(timer); }
      if (signal?.aborted) throw new RequestError("request body cancelled");
      if (item.done) { finished = true; return; }
      if (phase?.mode === "pack" && !managed && phase.rate) phase.credit = Math.min(phase.rate * phase.windowMs / 1000, phase.credit - (Date.now() - began) * phase.rate / 1000 + item.value.byteLength);
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
