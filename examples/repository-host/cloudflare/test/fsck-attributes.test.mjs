import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { entry, object, pack, receiveReport, wire } from "./fixture.mjs";
import { headers, start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} validates designated attributes including previously published blobs`, { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-fsck-attributes-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) for (const source of ["packed", "published", "delta"]) for (const length of [2047, 2048]) {
      const published = source !== "packed";
      const namespace = `attributes-${format}-${randomUUID()}`;
      const init = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(init.status, 200); await init.arrayBuffer();
      const value = object(format, 3, Buffer.alloc(length, 120));
      const base = source === "delta" ? object(format, 3, Buffer.alloc(2046, 120)) : value;
      const send = async (items, target, name) => {
        const response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, pack(format, items), target, name) });
        assert.equal(response.status, 200); return receiveReport(await response.arrayBuffer());
      };
      if (published) assert.match(await send([entry(3, base.body)], base.id, "refs/tags/raw"), /unpack ok/);
      const tree = object(format, 2, Buffer.concat([Buffer.from("100644 .gitattributes\0"), value.id]));
      // Copy the external 2046-byte base, then append one or two literal bytes.
      const delta = source === "delta" ? entry(7, Buffer.concat([Buffer.from([0xfe, 0x0f, length & 127 | 128, length >> 7, 0xb0, 0xfe, 7, length - 2046]), Buffer.alloc(length - 2046, 120)]), base.id) : null;
      const report = await send([...(delta ? [delta] : published ? [] : [entry(3, value.body)]), entry(2, tree.body)], tree.id, "refs/tags/tree");
      const accepted = length < 2048;
      assert.match(report, accepted ? /unpack ok/ : /unpack invalid/);
      const response = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() });
      const state = await response.json();
      assert.equal(state.refs, Number(published) + Number(accepted));
      assert.equal(state.objects, accepted ? source === "delta" ? 3 : 2 : Number(published));
      assert.equal(state.stages.length, Number(published) + Number(accepted));
      assert.equal(state.quarantineRows, 0); assert.equal(state.treeNameScratchRows, 0); assert.equal(state.temporaryObjects, 0);
      if (published) assert.ok(state.usage.r2Gets > 0, "published designated blob must be read and checked");
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
