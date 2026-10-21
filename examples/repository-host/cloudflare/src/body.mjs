// HTTP content coding wraps the Git byte stream. The Rust consumers enforce
// decoded control/pack limits; this bound also caps compressed transport input.
export function decodedBody(request, maximum, label) {
  const coding = request.headers.get("content-encoding")?.toLowerCase();
  if (!coding || coding === "identity") return request.body;
  if ((coding !== "gzip" && coding !== "x-gzip") || !request.body) throw new Error("unsupported HTTP content coding");
  let remaining = BigInt(maximum);
  return request.body.pipeThrough(new TransformStream({
    transform(chunk, controller) {
      remaining -= BigInt(chunk.byteLength);
      if (remaining < 0n) throw new Error(`compressed ${label} exceed byte limit`);
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
          new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("request body idle")), idleMs); }),
        ]);
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
