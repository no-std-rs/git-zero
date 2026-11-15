// A response owns repository scratch until its generator has finished cleanup.
// Bound idle backpressure, while allowing an actively drained pack to run long.
export function streamResponse(generator, type, idleMs = 60000, length, totalMs = 0) {
  let finish, controller, timer, totalTimer, stopping, closed = false;
  const done = new Promise((resolve) => { finish = resolve; });
  const complete = () => { closed = true; clearTimeout(timer); clearTimeout(totalTimer); finish(); };
  const stop = (error) => {
    if (stopping) return stopping;
    closed = true; clearTimeout(timer); clearTimeout(totalTimer);
    if (error) controller.error(error);
    stopping = (async () => { try { await generator.return(); } finally { complete(); } })();
    return stopping;
  };
  const body = new ReadableStream({
    start(value) { controller = value; if (totalMs) totalTimer = setTimeout(() => { stop(new Error("response time limit exceeded")).catch(() => {}); }, totalMs); },
    async pull() {
      if (closed) return;
      clearTimeout(timer);
      try {
        const next = await generator.next();
        if (closed) return;
        if (next.done) { controller.close(); complete(); }
        else {
          controller.enqueue(next.value);
          timer = setTimeout(() => { stop(new Error("response reader idle")).catch(() => {}); }, idleMs);
        }
      } catch (error) { if (!closed) await stop(error); }
    },
    cancel() { return stop(); },
  });
  // workerd derives Content-Length from FixedLengthStream, ignoring a manually
  // supplied length for an ordinary ReadableStream. Cancellation propagates to
  // the producer, including its scratch cleanup.
  let output = body;
  if (length !== undefined && typeof FixedLengthStream !== "undefined") {
    const fixed = new FixedLengthStream(length);
    body.pipeTo(fixed.writable).catch(() => {});
    output = fixed.readable;
  }
  return { response: new Response(output, { headers: { "Content-Type": type, "Cache-Control": "no-store", ...(length === undefined ? {} : { "Content-Length": String(length) }) } }), done };
}
