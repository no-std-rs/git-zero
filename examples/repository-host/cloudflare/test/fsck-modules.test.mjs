import { combinations } from "./combinations.mjs";
import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { entry, object, pack, receiveReport, wire } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

function size(value) {
  const bytes = [];
  do { const byte = value & 127; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value);
  return Buffer.from(bytes);
}
function literalDelta(base, result) {
  const parts = [size(base.length), size(result.length)];
  for (let at = 0; at < result.length; at += 127) {
    const part = result.subarray(at, at + 127); parts.push(Buffer.from([part.length]), part);
  }
  return Buffer.concat(parts);
}

testRuntimes("{runtime} validates designated gitmodules from current and earlier packs", { timeout: 120000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-fsck-modules-"));
  let server;
  try {
    server = await serve(join(directory, "state"));
    const cases = [
      ['[submodule "ok"]\nurl=https://example.test/r\n', true, false],
      ['[submodule "../bad"]\nurl=safe\n', false, false],
      ['[submodule "ok"]\nurl=-bad\n', false, false],
      ['[submodule "ok"]\npath=-bad\n', false, false],
      ['[submodule "ok"]\nupdate=!echo bad\n', false, false],
      ['[broken\n[submodule "ok"]\nurl=-bad\n', true, false],
      ['[submodule "ok"]\nurl="-bad\n', true, false],
      ['[submodule "ok"]\nurl=ssh://host/%0a\n', true, false],
      ['[submodule "ok"]\nurl=-bad\n', false, true],
      ['x'.repeat(2048), false, true],
      ['[submodule "ok"]\nurl=safe\n', true, true],
    ];
    for (const [format, source] of combinations([0, 1], ["packed", "published", "delta"])) for (const [body, accepted, both] of cases) {
      const namespace = `modules-${format}-${randomUUID()}`, published = source !== "packed";
      const value = object(format, 3, body), base = source === "delta" ? object(format, 3, '[submodule "base"]\nurl=safe\n') : value;
      const init = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(init.status, 200); await init.arrayBuffer();
      const send = async (items, target, name) => {
        const response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, pack(format, items), target, name) });
        assert.equal(response.status, 200); return receiveReport(await response.arrayBuffer());
      };
      if (published) assert.match(await send([entry(3, base.body)], base.id, "refs/tags/raw"), /unpack ok/);
      const names = both ? [".gitattributes", ".gitmodules"] : [".gitmodules"];
      const tree = object(format, 2, Buffer.concat(names.map((name) => Buffer.concat([Buffer.from(`100644 ${name}\0`), value.id]))));
      const encoded = source === "delta" ? [entry(7, literalDelta(base.body, value.body), base.id)] : published ? [] : [entry(3, value.body)];
      assert.match(await send([...encoded, entry(2, tree.body)], tree.id, "refs/tags/tree"), accepted ? /unpack ok/ : /unpack invalid/);
      const snapshot = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }); const state = await snapshot.json();
      assert.equal(state.refs, Number(published) + Number(accepted));
      assert.equal(state.objects, accepted ? source === "delta" ? 3 : 2 : Number(published));
      assert.equal(state.stages.length, Number(published) + Number(accepted));
      assert.equal(state.quarantineRows, 0); assert.equal(state.treeNameScratchRows, 0); assert.equal(state.temporaryObjects, 0);
      if (published) assert.ok(state.usage.r2Gets > 0, "earlier pack must be read to validate the designated body");
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
