import assert from "node:assert/strict";
import { test } from "node:test";
import { streamResponse } from "../src/response.mjs";

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test("stalled response releases its queue only after generator cleanup", async () => {
  let cleanup, release, completed = false;
  const cleaning = new Promise((resolve) => { cleanup = resolve; });
  const held = new Promise((resolve) => { release = resolve; });
  async function* chunks() {
    try { for (;;) yield new Uint8Array([1]); }
    finally { cleanup(); await held; }
  }
  const { response, done } = streamResponse(chunks(), "application/octet-stream", 10);
  const queued = done.then(() => { completed = true; });
  await Promise.race([cleaning, delay(1000).then(() => { throw new Error("idle response retained repository queue"); })]);
  assert.equal(completed, false, "scratch must remain owned while cleanup is pending");
  release(); await queued;
  await assert.rejects(response.arrayBuffer(), /response reader idle/);
});

test("active readers reset the idle deadline and preserve every byte", async () => {
  let cleaned = false;
  async function* chunks() {
    try { for (let i = 0; i < 8; i++) yield new Uint8Array([i]); }
    finally { cleaned = true; }
  }
  const { response, done } = streamResponse(chunks(), "application/octet-stream", 100);
  const reader = response.body.getReader(), bytes = [];
  for (;;) {
    const next = await reader.read(); if (next.done) break;
    bytes.push(...next.value); await delay(20);
  }
  await done;
  assert.deepEqual(bytes, [0, 1, 2, 3, 4, 5, 6, 7]); assert.equal(cleaned, true);
});

test("reader cancellation and generator failures both settle ownership", async () => {
  let cleaned = false;
  async function* chunks() { try { yield new Uint8Array([1]); yield new Uint8Array([2]); } finally { cleaned = true; } }
  const cancelled = streamResponse(chunks(), "application/octet-stream");
  const reader = cancelled.response.body.getReader(); await reader.read(); await reader.cancel();
  await cancelled.done; assert.equal(cleaned, true);
  async function* broken() { throw new Error("storage failed"); }
  const failed = streamResponse(broken(), "application/octet-stream");
  await assert.rejects(failed.response.arrayBuffer(), /storage failed/); await failed.done;
});
