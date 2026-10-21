import assert from "node:assert/strict";
import { gzipSync } from "node:zlib";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { project, receiveReport } from "./fixture.mjs";
import { headers, chunked, start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} receives HTTP gzip streams and rejects corrupt transport trailers before publication`, { timeout: 120000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-receive-coding-")); let server;
  try {
    server = await serve(directory);
    for (const format of [0, 1]) {
      const fixture = project(format), compressed = gzipSync(fixture.wire);
      for (const coding of ["gzip", "GZip", "x-gzip", "identity"]) {
        const namespace = `coding-${format}-${coding}`;
        const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(initialized.status, 200); await initialized.arrayBuffer();
        const response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: { ...headers(), "content-encoding": coding }, body: chunked(coding === "identity" ? fixture.wire : compressed, 7), duplex: "half" });
        assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs\/heads\/main/);
        const status = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }); const data = await status.json(); assert.equal(status.status, 200); assert.equal(data.refs, 1); assert.equal(data.objects, fixture.objects.length);
      }
      const namespace = `invalid-coding-${format}`;
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const corrupted = Buffer.from(compressed); corrupted[corrupted.length - 8] ^= 1;
      for (const [coding, body] of [["gzip", compressed.subarray(0, compressed.length - 1)], ["gzip", corrupted], ["deflate", compressed]]) {
        const response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: { ...headers(), "content-encoding": coding }, body });
        const bytes = await response.arrayBuffer(); assert.ok(response.status === 400 || (response.status === 200 && receiveReport(bytes).includes("unpack invalid")));
        const status = await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }); const data = await status.json(); assert.equal(status.status, 200); assert.equal(data.refs, 0); assert.equal(data.objects, 0); assert.deepEqual(data.stages, []); assert.equal(data.temporaryChunks, 0);
      }
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
