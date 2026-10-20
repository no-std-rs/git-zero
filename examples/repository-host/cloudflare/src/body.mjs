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
