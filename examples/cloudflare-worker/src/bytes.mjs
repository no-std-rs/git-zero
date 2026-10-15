// Bound complete host input before handing it to the fixed-memory Wasm ABI.
export async function readBytes(stream, maximum) {
  const reader = stream.getReader();
  const bytes = new Uint8Array(maximum);
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return bytes.subarray(0, length);
      if (length + value.length > maximum) throw new Error("Input exceeds limit");
      bytes.set(value, length);
      length += value.length;
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  } finally {
    reader.releaseLock();
  }
}

export async function readRequestBody(request, maximum) {
  const declared = request.headers.get("content-length");
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > maximum)) throw new Error("Request too large");
  if (!request.body) return new Uint8Array();
  const encoding = request.headers.get("content-encoding")?.toLowerCase();
  if (encoding && !["gzip", "identity"].includes(encoding)) throw new Error("Unsupported content encoding");
  let wireLength = 0;
  const bounded = request.body.pipeThrough(new TransformStream({
    transform(chunk, controller) {
      wireLength += chunk.length;
      if (wireLength > maximum) throw new Error("Request too large");
      controller.enqueue(chunk);
    },
  }));
  const stream = encoding === "gzip" ? bounded.pipeThrough(new DecompressionStream("gzip")) : bounded;
  return readBytes(stream, maximum);
}
