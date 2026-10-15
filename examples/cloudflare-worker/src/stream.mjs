const CONTENT_TYPES = [
  "text/plain; charset=utf-8",
  "application/x-git-upload-pack-advertisement",
  "application/x-git-upload-pack-result",
  "application/x-git-loose-object",
];

export function streamResponse(exports, result) {
  if (result < 0) {
    exports.stream_cancel();
    return new Response("Invalid or oversized Git request\n", {
      status: result === -1 ? 413 : 400, headers: { "Cache-Control": "no-store" },
    });
  }
  const output = new Uint8Array(exports.memory.buffer, exports.output_ptr(), 3);
  const status = new DataView(output.buffer, output.byteOffset, 3).getUint16(0, true);
  const type = CONTENT_TYPES[output[2]];
  const stream = new ReadableStream({
    pull(controller) {
      const count = exports.stream_read(16384);
      if (count < 0) {
        exports.stream_cancel();
        controller.error(new Error("Git response failed"));
      } else if (count === 0) {
        exports.stream_cancel();
        controller.close();
      } else {
        // Rust reuses OUTPUT on each pull; queued JS chunks own their bytes.
        controller.enqueue(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count).slice());
      }
    },
    cancel() { exports.stream_cancel(); },
  }, { highWaterMark: 0 });
  return new Response(stream, {
    status, headers: { "Content-Type": type, "Cache-Control": "no-store" },
  });
}
