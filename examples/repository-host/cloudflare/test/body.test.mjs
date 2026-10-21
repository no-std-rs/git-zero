import assert from "node:assert/strict";
import { test } from "node:test";
import { gzipSync } from "node:zlib";
import { requestBody, decodedBody } from "../src/body.mjs";

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test("HTTP gzip transport counts encoded bytes independently of decoded content", async () => {
  const plain = Buffer.alloc(32769, 97), encoded = gzipSync(plain);
  for (const coding of ["gzip", "GZIP", "x-gzip"]) {
    const request = new Request("http://fixture/receive", { method: "POST", headers: { "content-encoding": coding }, body: encoded });
    assert.deepEqual(Buffer.from(await new Response(decodedBody(request, encoded.length, "receive bytes")).arrayBuffer()), plain);
  }
  const oversized = new Request("http://fixture/receive", { method: "POST", headers: { "content-encoding": "gzip" }, body: encoded });
  await assert.rejects(new Response(decodedBody(oversized, BigInt(encoded.length - 1), "receive bytes")).arrayBuffer(), /compressed receive bytes exceed byte limit/);
});

test("HTTP transport rejects unsupported codings and corrupt gzip footers", async () => {
  const encoded = gzipSync(Buffer.from("verified control and pack bytes"));
  const unsupported = new Request("http://fixture/receive", { method: "POST", headers: { "content-encoding": "deflate" }, body: encoded });
  assert.throws(() => decodedBody(unsupported, encoded.length, "receive bytes"), /unsupported HTTP content coding/);
  encoded[encoded.length - 8] ^= 1;
  const corrupt = new Request("http://fixture/receive", { method: "POST", headers: { "content-encoding": "gzip" }, body: encoded });
  await assert.rejects(new Response(decodedBody(corrupt, encoded.length, "receive bytes")).arrayBuffer());
});

test("idle body reads settle even when transport cancellation never resolves", { timeout: 1000 }, async () => {
  let cancelled = false;
  const stream = new ReadableStream({ cancel() { cancelled = true; return new Promise(() => {}); } });
  const body = requestBody(stream, 10);
  await assert.rejects(body.next(), /request body idle/);
  assert.equal(cancelled, true); assert.equal(stream.locked, false);
});

test("body deadlines reset for each chunk and exclude consumer work", async () => {
  let next = 0;
  const stream = new ReadableStream({ async pull(controller) {
    await delay(10);
    if (next === 4) controller.close(); else controller.enqueue(new Uint8Array([next++]));
  } });
  const bytes = [];
  for await (const chunk of requestBody(stream, 50)) { bytes.push(...chunk); await delay(60); }
  assert.deepEqual(bytes, [0, 1, 2, 3]); assert.equal(stream.locked, false);
});

test("body consumers release their reader on early return and transport errors", async () => {
  let cancelled = false;
  const stream = new ReadableStream({ pull(controller) { controller.enqueue(new Uint8Array([1])); }, cancel() { cancelled = true; } });
  for await (const chunk of requestBody(stream)) { assert.equal(chunk[0], 1); break; }
  assert.equal(cancelled, true); assert.equal(stream.locked, false);
  const failed = new ReadableStream({ start(controller) { controller.error(new Error("transport lost")); } });
  await assert.rejects(requestBody(failed).next(), /transport lost/);
  assert.equal(failed.locked, false);
});
