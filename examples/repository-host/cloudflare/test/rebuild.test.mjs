import test from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { start, headers } from "./wrangler.mjs";
const execute = promisify(execFile), root = fileURLToPath(new URL("../", import.meta.url));
test("a byte-identical codec rebuild does not reload a live Wrangler repository", { timeout: 30000 }, async () => {
  const dir = await mkdtemp(join(tmpdir(), "git-zero-rebuild-")); let server;
  try {
    server = await start(dir);
    const initialized = await fetch(`${server.origin}/_test/rebuild/initialize`, { method: "POST", headers: headers() }); assert.equal(initialized.status, 200); await initialized.arrayBuffer();
    const state = async () => { const response = await fetch(`${server.origin}/_test/rebuild/state`, { headers: headers() }); assert.equal(response.status, 200); return response.json(); };
    const before = await state();
    await execute(process.execPath, ["scripts/build.mjs"], { cwd: root, timeout: 20000 });
    for (let i = 0; i < 20; i++) { await new Promise((resolve) => setTimeout(resolve, 100)); assert.equal((await state()).epoch, before.epoch, "unchanged build output must preserve the active isolate"); }
  } finally { await server?.stop(); await rm(dir, { recursive: true, force: true }); }
});
