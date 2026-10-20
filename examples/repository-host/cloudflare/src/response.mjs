// A response owns repository scratch until its generator has finished cleanup.
// Bound idle backpressure, while allowing an actively drained pack to run long.
export function streamResponse(generator, type, idleMs = 60000) {
  let finish, controller, timer, stopping, closed = false;
  const done = new Promise((resolve) => { finish = resolve; });
  const complete = () => { closed = true; clearTimeout(timer); finish(); };
  const stop = (error) => {
    if (stopping) return stopping;
    closed = true; clearTimeout(timer);
    if (error) controller.error(error);
    stopping = (async () => { try { await generator.return(); } finally { complete(); } })();
    return stopping;
  };
  const body = new ReadableStream({
    start(value) { controller = value; },
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
  return { response: new Response(body, { headers: { "Content-Type": type, "Cache-Control": "no-store" } }), done };
}
