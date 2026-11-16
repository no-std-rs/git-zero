import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { start } from "./wrangler.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
test("native stub identity honors an emulated EU jurisdiction binding and refuses the global scope", { timeout: 60000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-eu-")); let server;
  try {
    const config = join(root, "wrangler.toml");
    await writeFile(config, `name = "git-zero-eu-test"\nmain = ${JSON.stringify(fileURLToPath(new URL("./fixtures/eu.mjs", import.meta.url)))}\ncompatibility_date = "2026-10-06"\nsend_metrics = false\n[vars]\nKEY = "${GRANT_KEY}"\n[[r2_buckets]]\nbinding = "DATA"\nbucket_name = "eu-fixture-data"\n[[durable_objects.bindings]]\nname = "EU_REPOS"\nclass_name = "LocalEURepository"\n[[migrations]]\ntag = "v1"\nnew_sqlite_classes = ["LocalEURepository"]\n`);
    server = await start(join(root, "state"), { config, hostToken: null });
    const remote = `${server.origin}/550e8400-e29b-41d4-a716-446655440000.git`, headers = { "x-git-zero-grant": mint("550e8400-e29b-41d4-a716-446655440000", "admin"), "content-type": "application/json" };
    const created = await fetch(`${remote}/api/v1/repository`, { method: "PUT", headers, body: "{}" }); assert.equal(created.status, 201, await created.clone().text()); await created.arrayBuffer();
    const direct = await fetch(`${remote}/api/v1/refs?direct=eu`, { headers }); assert.equal(direct.status, 200, await direct.clone().text()); assert.deepEqual((await direct.json()).refs, []);
    const wrong = await fetch(`${remote}/api/v1/refs?direct=wrong`, { headers }); assert.equal(wrong.status, 404); await wrong.arrayBuffer();
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
