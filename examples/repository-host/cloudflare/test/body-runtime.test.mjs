import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { request } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { headers } from "./wrangler.mjs";

async function entered(directory, namespace) {
  for (const file of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, file.name);
    if (file.isDirectory()) { if (await entered(path, namespace)) return true; continue; }
    if (!file.name.endsWith(".sqlite")) continue;
    const database = new DatabaseSync(path, { readOnly: true });
    try {
      if (!database.prepare("SELECT 1 FROM sqlite_master WHERE name='host_identity'").get()) continue;
      if (database.prepare("SELECT namespace FROM host_identity WHERE id=1").get()?.namespace === namespace) return true;
    } catch (error) {
      // SQLite's low byte is the primary code; WAL recovery can return the
      // extended SQLITE_BUSY_RECOVERY (261) while this read-only probe starts.
      const primary = error.errcode & 255;
      if (primary !== 5 && primary !== 6) throw error;
    } finally { database.close(); }
  }
  return false;
}

async function waitForHandler(directory, namespace, transport, server) {
  for (let attempt = 0; attempt < 300; attempt++) {
    if (transport.error) throw transport.error;
    assert.equal(transport.status, null, `partial POST returned before entering ${namespace}\n${server.logs()}`);
    if (await entered(directory, namespace)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  assert.fail(`partial POST never entered ${namespace}\n${server.logs()}`);
}

testRuntimes("{runtime} releases queues held by idle request bodies", { timeout: 120000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-idle-body-"));
  const clients = []; let server;
  try {
    server = await serve(directory);
    await Promise.all([["git-receive-pack", null, "000"], ["git-upload-pack", null, "000"], ["git-receive-pack", "gzip", Buffer.from([31, 139, 8])]].map(async ([service, coding, prefix]) => {
      const namespace = `body-${service}-${coding ?? "identity"}-${randomUUID()}`;
      const transport = { error: null, status: null };
      const client = request(`${server.origin}/${namespace}.git/${service}`, { method: "POST", headers: { ...headers(), "content-type": `application/x-${service}-request`, ...(coding ? { "content-encoding": coding } : {}) } }, (response) => { transport.status = response.statusCode; response.resume(); });
      clients.push(client); client.on("error", (error) => { transport.error = error; });
      client.write(prefix); // Leave the pkt-line or gzip header and HTTP body open.
      await waitForHandler(directory, namespace, transport, server);
      const waiting = Date.now();
      let completed = false;
      const queued = fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers(), signal: AbortSignal.timeout(75000) }).then(async (response) => {
        assert.equal(response.status, 200); const state = await response.json(); completed = true; return state;
      });
      await new Promise((resolve) => setTimeout(resolve, 100));
      assert.equal(completed, false, "the open POST must actually hold the repository queue");
      const state = await queued;
      assert.ok(Date.now() - waiting >= 55000, "the configured input deadline, rather than a worker reload, must release the queue");
      assert.equal(state.refs, 0); assert.equal(state.objects, 0); assert.deepEqual(state.stages, []);
    }));
  } finally { for (const client of clients) client.destroy(); await server?.stop(); await rm(directory, { recursive: true, force: true }); }
}, { frozen: true });
