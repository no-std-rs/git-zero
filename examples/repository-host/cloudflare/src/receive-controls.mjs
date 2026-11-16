import { Codec } from "./codec.mjs";
import { decodedBody, requestBody, bodyBudget, bindBodyBudget } from "./body.mjs";
import { RequestError } from "./errors.mjs";
import { PAGE_BYTES, bytes } from "./storage.mjs";

// Rust validates controls under the small codec envelope. Persist only their
// bounded wire prefix, release that codec, then replay into an independently
// admitted heavy intake. No idle prefix owns maximum push caches or a stage.
export async function receiveControls(store, module, format, request, owner, capabilities) {
  let codec = new Codec(module, format, { ...store.env.CODEC_LIMITS, capabilities });
  codec.receiveInit();
  const limits = store.env.CODEC_LIMITS, budget = bodyBudget(request);
  const maximum = 2n * (BigInt(limits.maxPackBytes) + BigInt(limits.maxControlBytes));
  const input = requestBody(decodedBody(request, maximum, "receive bytes"), 60000, budget);
  let seq = 0, tail, ready = false;
  try {
    while (!ready) {
      const next = await input.next(); if (next.done) throw new RequestError("incomplete receive controls");
      let offset = 0;
      while (offset < next.value.length) {
        const part = next.value.subarray(offset, offset + PAGE_BYTES), event = codec.receiveStep(part, false);
        if (event.consumed) store.exec("INSERT INTO job_controls(job,seq,data) VALUES(?,?,?)", owner.id, seq++, new Uint8Array(part.subarray(0, event.consumed)).buffer);
        offset += event.consumed;
        if (event.tag === 6) { tail = next.value.subarray(offset); ready = true; break; }
        if (!event.consumed && !event.written && event.tag === 1) throw new RequestError("receive controls made no progress");
      }
    }
  } catch (error) { await input.return().catch(() => {}); store.exec("DELETE FROM job_controls WHERE job=?", owner.id); throw error; }
  finally { codec.receiveCancel(); codec = null; }
  let after = -1, prefix = true, closed = false, controlsReleased = false;
  const clearControls = () => { if (!controlsReleased) { store.exec("DELETE FROM job_controls WHERE job=?", owner.id); controlsReleased = true; } };
  const stop = async () => { if (closed) return; closed = true; tail = null; clearControls(); await input.return().catch(() => {}); };
  // Policy rejection transfers the transport tail to the off-job bounded drain.
  owner.finalizers.add(() => owner.drain ? undefined : stop());
  const body = new ReadableStream({
    async pull(controller) {
      try {
        if (prefix) {
          const row = store.exec("SELECT seq,data FROM job_controls WHERE job=? AND seq>? ORDER BY seq LIMIT 1", owner.id, after)[0];
          if (row) { after = row.seq; controller.enqueue(bytes(row.data)); return; }
          prefix = false; clearControls();
        }
        if (tail?.length) { const data = tail.subarray(0, PAGE_BYTES); tail = tail.subarray(data.length); controller.enqueue(data); return; }
        const part = await input.next();
        if (part.done) { controller.close(); await stop(); } else controller.enqueue(part.value);
      } catch (error) { controller.error(error); await stop(); }
    },
    cancel: stop,
  });
  const headers = new Headers(request.headers); headers.delete("content-encoding"); headers.delete("content-length");
  const replay = new Request(request.url, { method: request.method, headers, body, duplex: "half" });
  bindBodyBudget(replay, { BODY_TOTAL_MS: budget.totalMs, BODY_DEADLINE: budget.deadline, BODY_SIGNAL: budget.signal });
  return replay;
}
