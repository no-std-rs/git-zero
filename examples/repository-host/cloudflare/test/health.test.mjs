import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";
import { gateway } from "./host.mjs";

test("health refuses readiness without the required authentication secret", async () => {
  for (const HOST_TOKEN of [undefined, ""]) {
    const response = await gateway.fetch(new Request("http://fixture/health"), { HOST_TOKEN });
    assert.equal(response.status, 503); const result = await response.json();
    assert.equal(result.ready, false); assert.equal(result.gitReceive, false); assert.equal(result.gitFetch, false);
  }
  const response = await gateway.fetch(new Request("http://fixture/health"), { HOST_TOKEN: TOKEN });
  assert.equal(response.status, 200); assert.equal((await response.json()).ready, true);
});

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} requires authentication configuration before reporting readiness`, { timeout: 30000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-health-")); let server;
  try {
    for (const hostToken of [null, TOKEN]) {
      server = await serve(join(root, hostToken === null ? "unconfigured" : "configured"), { hostToken });
      const response = await fetch(`${server.origin}/health`), health = await response.json();
      assert.equal(response.status, hostToken === null ? 503 : 200); assert.equal(health.ready, hostToken !== null);
      assert.equal(health.gitReceive, hostToken !== null); assert.equal(health.gitFetch, hostToken !== null);
      const advertisement = await fetch(`${server.origin}/health-test.git/info/refs?service=git-upload-pack`, { headers: headers() });
      assert.equal(advertisement.status, hostToken === null ? 401 : 200); await advertisement.arrayBuffer();
      await server.stop(); server = null;
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
