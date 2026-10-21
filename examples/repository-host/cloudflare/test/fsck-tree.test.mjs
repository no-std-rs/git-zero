import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { entry, object, pack, receiveReport, wire } from "./fixture.mjs";
import { headers, start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} preserves strict tree policy and historical warnings`, { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-fsck-tree-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    const cases = [
      [".gitmodules", "120000", false], ["gitmod~1", "120000", false],
      ["GI7EBA~1", "40000", false], ["a\\.gitmodules", "40000", false],
      [".gitattributes", "40000", false], ["gitatt~1", "40000", false],
      ["GI7D29~1", "40000", false], [".gi\u200ctattributes", "40000", false],
      [".gitattributes", "120000", true], [".gitignore", "120000", true],
      ["file", "100664", true], ["file", "0100644", false], ["file", "0100664", false],
      ["directory", "040000", false], ["x".repeat(4096), "100644", true], ["x".repeat(4097), "100644", false],
    ];
    for (const format of [0, 1]) for (const [name, mode, accepted] of cases) {
      const namespace = `tree-${format}-${randomUUID()}`;
      const target = mode.endsWith("40000") ? object(format, 2, "") : object(format, 3, "safe\n");
      const tree = object(format, 2, Buffer.concat([Buffer.from(`${mode} ${name}\0`), target.id]));
      const packed = pack(format, [target, tree].map((value) => entry(value.kind, value.body)));
      const init = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(init.status, 200); await init.arrayBuffer();
      const response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, packed, tree.id) });
      assert.equal(response.status, 200);
      assert.match(receiveReport(await response.arrayBuffer()), accepted ? /unpack ok/ : /unpack invalid/);
      const snapshot = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() });
      const state = await snapshot.json();
      assert.equal(state.refs, Number(accepted)); assert.equal(state.objects, accepted ? 2 : 0);
      assert.equal(state.stages.length, Number(accepted)); assert.equal(state.treeNameScratchRows, 0);
      if (!accepted) assert.equal(state.stagedChunks, 0);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
