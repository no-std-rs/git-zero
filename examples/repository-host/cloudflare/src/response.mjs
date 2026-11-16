// A response owns repository scratch until its generator has finished cleanup.
// Bound idle backpressure, while allowing an actively drained pack to run long.
export function streamResponse(generator, type, idleMs = 60000, length, totalMs = 0, { minimumBytesPerSecond = 0, throughputWindowMs = 30000, signal, onStop } = {}) {
  let finish, controller, timer, totalTimer, rateTimer, stopping, closed = false, sent = 0, sampled = 0;
  const abort = () => { stop(signal?.reason?.name !== "AbortError" && signal?.reason?.message ? signal.reason : new Error("repository response cancelled")).catch(() => {}); };
  // Enter the generator before cancellation can arrive: return() on an
  // unstarted generator does not execute its finally/ownership cleanup.
  let first = generator.next(); first.catch(() => {});
  const done = new Promise((resolve) => { finish = resolve; });
  const complete = () => { closed = true; clearTimeout(timer); clearTimeout(totalTimer); clearTimeout(rateTimer); signal?.removeEventListener("abort", abort); finish(); };
  const stop = (error) => {
    if (stopping) return stopping;
    closed = true; clearTimeout(timer); clearTimeout(totalTimer); clearTimeout(rateTimer);
    if (error) controller.error(error);
    stopping = (async () => { try { await first?.catch(() => {}); await generator.return(); } finally { complete(); } })();
    onStop?.(error);
    return stopping;
  };
  const body = new ReadableStream({
    start(value) { controller = value;
      signal?.addEventListener("abort", abort, { once: true });
      if (signal?.aborted) { abort(); return; }
      if (minimumBytesPerSecond) {
        const sample = () => {
          if (closed) return;
          if (sent - sampled < minimumBytesPerSecond * throughputWindowMs / 1000) { stop(new Error("response minimum throughput not met")).catch(() => {}); return; }
          sampled = sent; rateTimer = setTimeout(sample, throughputWindowMs);
        };
        rateTimer = setTimeout(sample, throughputWindowMs);
      }
      if (totalMs) totalTimer = setTimeout(() => { stop(new Error("response time limit exceeded")).catch(() => {}); }, totalMs); },
    async pull() {
      if (closed) return;
      clearTimeout(timer);
      try {
        const next = await (first ?? generator.next()); first = null;
        if (closed) return;
        if (next.done) { controller.close(); complete(); }
        else {
          sent += next.value.byteLength; controller.enqueue(next.value);
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
