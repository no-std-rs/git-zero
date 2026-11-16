import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { project, receiveReport } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

async function json(response, status = 200) { const value = await response.json(); assert.equal(response.status, status, JSON.stringify(value)); return value; }
async function databasePath(directory, namespace) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) { const found = await databasePath(path, namespace); if (found) return found; }
    else if (entry.name.endsWith(".sqlite")) {
      const database = new DatabaseSync(path, { readOnly: true });
      try {
        if (database.prepare("SELECT 1 FROM sqlite_master WHERE name='host_identity'").get() && database.prepare("SELECT namespace FROM host_identity WHERE id=1").get()?.namespace === namespace) return path;
      } finally { database.close(); }
    }
  }
  return null;
}
testRuntimes("{runtime} creates both formats and reclaims R2 plus native SQLite without resurrection", { timeout: 60000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-lifecycle-")); let server;
  try {
    server = await serve(directory);
    for (const format of [0, 1]) {
      let remote = `${server.origin}/lifecycle-${format}.git`, endpoint = `${remote}/api/v1/repository`;
      const settings = { objectFormat: format ? "sha256" : "sha1", defaultBranch: "main" };
      const init = { method: "PUT", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify(settings) };
      const first = await json(await fetch(endpoint, init), 201);
      assert.deepEqual(await json(await fetch(endpoint, init)), first);
      const fixture = project(format), pushed = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: fixture.wire });
      assert.equal(pushed.status, 200); assert.match(receiveReport(await pushed.arrayBuffer()), /ok refs\/heads\/main/);
      const current = await json(await fetch(endpoint, { headers: headers() }));
      assert.equal(current.generation, 1); assert.equal(current.head.oid, fixture.root.id.toString("hex")); assert.equal(current.storedPackBytes, fixture.pack.length);
      const sqlite = await databasePath(directory, `lifecycle-${format}`); assert.ok(sqlite);
      let result;
      for (let n = 0; n < 10; n++) {
        const response = await fetch(endpoint, { method: "DELETE", headers: headers() });
        result = await json(response, response.status === 202 ? 202 : 200);
        if (result.complete) break;
      }
      assert.equal(result.complete, true);
      const cleared = new DatabaseSync(sqlite, { readOnly: true });
      try { assert.equal(cleared.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE name IN ('repository','stage_objects','stage_chunks','refs','host_identity')").get().n, 0); }
      finally { cleared.close(); }
      await server.stop(); server = await serve(directory);
      remote = `${server.origin}/lifecycle-${format}.git`; endpoint = `${remote}/api/v1/repository`;
      assert.equal((await fetch(`${remote}/info/refs?service=git-upload-pack`, { headers: headers() })).status, 404);
      assert.equal((await fetch(endpoint, init)).status, 410);
      assert.equal((await json(await fetch(endpoint, { method: "DELETE", headers: headers() }))).complete, true);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
