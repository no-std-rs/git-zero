// HTTP content coding wraps the Git byte stream. The Rust consumers enforce
// decoded control/pack limits; this bound also caps compressed transport input.
import { RequestError } from "./errors.mjs";
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
export async function* requestBody(stream, idleMs = 60000) {
  if (!stream) return;
  const reader = stream.getReader();
  try {
    for (;;) {
      let timer, item;
      try {
        item = await Promise.race([
          reader.read(),
          new Promise((_, reject) => { timer = setTimeout(() => reject(new RequestError("request body idle")), typeof idleMs === "function" ? idleMs() : idleMs); }),
        ]);
      } catch (error) {
        throw error instanceof RequestError ? error : new RequestError(String(error.message ?? error), { cause: error });
      } finally { clearTimeout(timer); }
      if (item.done) return;
      yield item.value;
    }
  } finally {
    // A transport's cancellation promise may itself wait for the stalled peer.
    // No abandoned read can resume the parser after this iterator has stopped.
    reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

// Refusals consume a bounded transport tail so a still-uploading Git client can
// receive the status instead of observing a reset. Never parse unauthorized data.
export async function refusal(request, response, env = {}) {
  if (!request.body || request.body.locked || request.bodyUsed) return response;
  const maximum = env.REJECTION_DRAIN_BYTES ?? 64 * 1024 * 1024;
  const deadline = Date.now() + (env.REJECTION_DRAIN_MS ?? 30000);
  let discarded = 0;
  try {
    for await (const part of requestBody(request.body, () => Math.max(1, Math.min(10000, deadline - Date.now())))) {
      discarded += part.byteLength;
      if (discarded > maximum || Date.now() >= deadline) break;
    }
  } catch { /* A vanished/over-bound peer cannot be made to read a refusal. */ }
  return response;
}
