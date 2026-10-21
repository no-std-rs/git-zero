import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { entry, object, pack, receiveReport, wire } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

function variable(value) {
  const bytes = [];
  do { const byte = value & 127; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value);
  return Buffer.from(bytes);
}

async function checkLargeModules(server, format) {
  const namespace = `large-${format}`, remote = `${server.origin}/${namespace}.git`;
  const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
  assert.equal(initialized.status, 200); await initialized.arrayBuffer();
  const base = object(format, 3, "#\n".repeat(512 * 1024));
  const value = object(format, 3, Buffer.concat(Array(9).fill(base.body)));
  const delta = Buffer.concat([variable(base.body.length), variable(value.body.length), Buffer.from(Array(9).fill([0xc0, 0x10]).flat())]);
  const tree = object(format, 2, Buffer.concat([".gitattributes", ".gitmodules"].map((name) => Buffer.concat([Buffer.from(`100644 ${name}\0`), value.id]))));
  const packed = pack(format, [entry(7, delta, base.id), entry(2, tree.body)]);
  const send = async (body) => {
    const response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body });
    assert.equal(response.status, 200); return receiveReport(await response.arrayBuffer());
  };
  assert.match(await send(wire(format, pack(format, [entry(3, base.body)]), base.id, "refs/tags/base")), /unpack ok/);
  const response = await fetch(`${server.origin}/_test/${namespace}/intake`, {
    method: "POST", headers: { ...headers(format), "x-test-temporary-bytes": String(2 * value.body.length) },
    body: wire(format, packed, tree.id, "refs/tags/combined"),
  });
  const text = await response.text(); assert.equal(response.status, 200, text);
  assert.equal(JSON.parse(text).budgets.temporaryReservedBytes, value.body.length);
  assert.match(await send(wire(format, packed, tree.id, "refs/tags/combined")), /ok refs\/tags\/combined/);
  const reused = object(format, 2, Buffer.concat([Buffer.from("100644 .gitmodules\0"), value.id]));
  assert.match(await send(wire(format, pack(format, [entry(2, reused.body)]), reused.id, "refs/tags/reused")), /ok refs\/tags\/reused/);
  const state = await (await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() })).json();
  assert.equal(state.refs, 3); assert.equal(state.objects, 4);
  assert.equal(state.temporaryObjects, 0); assert.equal(state.temporaryChunks, 0);
  assert.equal(state.lastIntakeBudget.temporaryReservedBytes, value.body.length);
}

testRuntimes("{runtime} rechecks large designated deltas without rewriting temporary bodies", { timeout: 120000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-large-designated-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      await checkLargeModules(server, format);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
