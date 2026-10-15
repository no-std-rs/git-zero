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
