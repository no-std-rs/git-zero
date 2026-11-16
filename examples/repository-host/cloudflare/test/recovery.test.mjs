import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { request } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { entry, object, pack, packet, wire } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

const TABLES = ["fetch_wants", "fetch_haves", "fetch_shallow", "fetch_probe", "fetch_common", "fetch_selected"];

async function frontierRows(directory) {
  let result;
  for (const file of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, file.name);
    if (file.isDirectory()) { result ??= await frontierRows(path); continue; }
    if (!file.name.endsWith(".sqlite")) continue;
    const database = new DatabaseSync(path, { readOnly: true });
    try {
      if (database.prepare("SELECT 1 FROM sqlite_master WHERE name='fetch_selected'").get()) {
        result = Object.fromEntries(TABLES.map((table) => {
          const privateTables = database.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB ?").all(`job_*_${table}`);
          return [table, [table, ...privateTables.map((row) => row.name)].reduce((total, name) => total + database.prepare(`SELECT COUNT(*) AS count FROM ${name}`).get().count, 0)];
        }));
      }
    } finally { database.close(); }
  }
  return result;
}

async function pausedFetch(server, namespace) {
  const value = object(0, 3, randomBytes(32 * 1024 * 1024));
  const published = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(0, pack(0, [entry(3, value.body)]), value.id) });
  assert.equal(published.status, 200); await published.arrayBuffer();
  const controls = Buffer.concat([packet(`want ${value.id.toString("hex")} side-band-64k object-format=sha1\n`), Buffer.from("0000"), packet("done\n")]);
  let client;
  const response = await new Promise((resolve, reject) => {
    client = request(`${server.origin}/${namespace}.git/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request" } }, (incoming) => { incoming.on("error", () => {}); incoming.pause(); resolve(incoming); });
    client.once("error", reject); client.end(controls);
  });
  assert.equal(response.statusCode, 200);
  if (!response.readableLength) await once(response, "readable");
  return { client, response };
}

testRuntimes("{runtime} reclaims fetch frontiers after a hard process crash", { timeout: 60000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-fetch-recovery-"));
  let server, client, response;
  try {
    server = await serve(directory);
    ({ client, response } = await pausedFetch(server, "recovery"));
    await server.crash(); server = null;
    response.destroy(); client.destroy();
    const abandoned = await frontierRows(directory);
    assert.ok(abandoned.fetch_wants > 0 && abandoned.fetch_selected > 0, "the crash must leave a durable in-progress frontier, rather than a completed fetch");
    server = await serve(directory);
    // Recovery must run on the first read, without relying on another fetch POST.
    const state = await fetch(`${server.origin}/_test/recovery/state`, { headers: headers() });
    assert.equal(state.status, 200); const recovered = await state.json();
    assert.equal(recovered.refs, 1); assert.equal(recovered.objects, 1);
    assert.deepEqual(await frontierRows(directory), Object.fromEntries(TABLES.map((table) => [table, 0])));
  } finally { response?.destroy(); client?.destroy(); await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});

testRuntimes("{runtime} serves browse beside an idle response and reclaims its private frontier at the idle deadline", { timeout: 90000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-idle-response-"));
  let server, client, response;
  try {
    server = await serve(directory);
    ({ client, response } = await pausedFetch(server, "idle"));
    const pending = await frontierRows(directory);
    assert.ok(pending.fetch_selected > 0, "the unread response must retain an active frontier");
    // Leave the socket open. Browse must complete immediately, while only the
    // unchanged server idle deadline retires this job's private scratch.
    const started = Date.now();
    const inspect = async () => {
      const state = await fetch(`${server.origin}/_test/idle/state`, { headers: headers(), signal: AbortSignal.timeout(3000) });
      assert.equal(state.status, 200); return state.json();
    };
    const ready = await inspect(); assert.equal(ready.refs, 1); assert.equal(ready.objects, 1);
    assert.ok(Date.now() - started < 3000, "browse cannot wait behind the paused fetch");
    assert.ok((await frontierRows(directory)).fetch_selected > 0, "browse must not clear live scratch");
    await new Promise((resolve) => setTimeout(resolve, 55000));
    assert.ok((await frontierRows(directory)).fetch_selected > 0, "preserve the original 60-second reader idle deadline");
    const deadline = started + 75000;
    while ((await frontierRows(directory)).fetch_selected > 0 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 100));
    assert.deepEqual(await frontierRows(directory), Object.fromEntries(TABLES.map((table) => [table, 0])));
  } finally { response?.destroy(); client?.destroy(); await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
