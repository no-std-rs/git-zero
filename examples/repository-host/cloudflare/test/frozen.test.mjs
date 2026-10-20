import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { start } from "./wrangler.mjs";

test("frozen Wrangler jobs keep their worker until explicit restart", { timeout: 30000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-frozen-"));
  const config = join(directory, "wrangler.toml"), persist = join(directory, "state");
  const module = (label) => `let epoch; export default {fetch(){epoch??=crypto.randomUUID();return Response.json({epoch,label:${JSON.stringify(label)}})}}`;
  let server;
  try {
    await writeFile(config, 'name="git-zero-frozen-test"\nmain="streamed.js"\ncompatibility_date="2026-10-06"\nsend_metrics=false\n');
    await writeFile(join(directory, "streamed.js"), module("before"));
    server = await start(persist, { config, cwd: directory, frozen: true });
    const read = async () => (await fetch(server.origin, { signal: AbortSignal.timeout(5000) })).json();
    const before = await read();
    await writeFile(join(directory, "streamed.js"), module("after"));
    await new Promise((resolve) => setTimeout(resolve, 1000));
    assert.deepEqual(await read(), before);
    assert.equal(server.logs().includes("Reloading local server"), false);
    await server.stop(); server = await start(persist, { config, cwd: directory, frozen: true });
    const restarted = await read();
    assert.equal(restarted.label, "after"); assert.notEqual(restarted.epoch, before.epoch);
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
