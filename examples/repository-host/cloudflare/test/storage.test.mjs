import assert from "node:assert/strict";
import { randomBytes, randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { CHUNK_BYTES } from "../src/storage.mjs";
import { deltas, entry, object, pack, packet, project, wire } from "./fixture.mjs";
import { chunked, headers, start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

async function responseJson(response) {
  const text = await response.text();
  assert.equal(response.status, 200, text);
  return JSON.parse(text);
}
async function upload(server, namespace, fixture, format) {
  return responseJson(await fetch(`${server.origin}/_test/${namespace}/intake`, { method: "POST", headers: headers(format), body: chunked(fixture.wire), duplex: "half" }));
}
async function rowFor(server, namespace, result, id) {
  const data = await responseJson(await fetch(`${server.origin}/_test/${namespace}/stage/${result.stage}`, { headers: headers() }));
  assert.equal(data.stage.phase, "verified");
  return data.objects.find((row) => row.oid === id.toString("hex"));
}
async function canonical(server, namespace, result, row, expected) {
  const response = await fetch(`${server.origin}/_test/${namespace}/object/${result.stage}?node=${row.node}&data=1`, { headers: headers() });
  assert.equal(response.status, 200, await response.clone().text());
  assert.equal(response.headers.get("x-object-id"), expected.id.toString("hex"));
  assert.deepEqual(Buffer.from(await response.arrayBuffer()), expected.body);
}

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} persists verified packs and reclaims failed quarantine`, { timeout: 120000 }, async () => {
  const persist = await mkdtemp(join(tmpdir(), "git-zero-own-worker-"));
  let server;
  const restart = [];
  try {
    server = await serve(persist);
    assert.equal((await fetch(`${server.origin}/_test/unauthorized/state`)).status, 401);
    assert.equal((await fetch(`${server.origin}/_services/register`, { method: "POST", headers: headers(), body: "{}" })).status, 404);
    for (const format of [0, 1]) {
      for (const forward of [false, true]) {
        const namespace = `delta-${format}-${forward}-${randomUUID()}`;
        const fixture = deltas(format, forward);
        const result = await upload(server, namespace, fixture, format);
        assert.equal(result.quarantine, true);
        assert.equal(result.refsUpdated, 0);
        assert.equal(result.objects, 3);
        assert.equal(result.packBytes, fixture.pack.length);
        assert.ok(result.wasmMemoryBytes <= 6 * 1024 * 1024);
        assert.ok(result.usage.r2Puts > 0);
        const row = await rowFor(server, namespace, result, fixture.root.id);
        assert.equal(row.depth, 2);
        await canonical(server, namespace, result, row, fixture.root);
        restart.push({ namespace, result, row, expected: fixture.root });
      }
      const namespace = `project-${format}-${randomUUID()}`;
      const fixture = project(format);
      const result = await upload(server, namespace, fixture, format);
      for (const expected of fixture.objects) await canonical(server, namespace, result, await rowFor(server, namespace, result, expected.id), expected);
      for (const [label, invalid] of [["missing", project(format, true)], ["alias", project(format, false, true)]]) {
        const name = `${label}-${format}-${randomUUID()}`;
        const response = await fetch(`${server.origin}/_test/${name}/intake`, { method: "POST", headers: headers(format), body: invalid.wire });
        assert.equal(response.status, 400, await response.text());
        const state = await responseJson(await fetch(`${server.origin}/_test/${name}/state`, { headers: headers() }));
        assert.equal(state.refs, 0); assert.equal(state.objects, 0);
        assert.equal(state.stages[0].phase, "failed");
      }
    }
    const expiredNamespace = `expired-${randomUUID()}`;
    const expired = await upload(server, expiredNamespace, project(0), 0);
    await responseJson(await fetch(`${server.origin}/_test/${expiredNamespace}/stage/${expired.stage}/expire`, { method: "POST", headers: headers() }));
    const pendingCleanup = await responseJson(await fetch(`${server.origin}/_test/${expiredNamespace}/state`, { headers: headers() }));
    assert.equal(pendingCleanup.stagedChunks, 1);
    const shallowNamespace = `shallow-only-${randomUUID()}`;
    const shallow = await upload(server, shallowNamespace, { wire: Buffer.concat([packet(`shallow ${project(0).root.id.toString("hex")}\n`), Buffer.from("0000")]) }, 0);
    await responseJson(await fetch(`${server.origin}/_test/${shallowNamespace}/stage/${shallow.stage}/expire`, { method: "POST", headers: headers() }));
    const before = restart[0].result.epoch;
    await server.stop(); server = await serve(persist);
    const recovered = await responseJson(await fetch(`${server.origin}/_test/${expiredNamespace}/state`, { headers: headers() }));
    assert.equal(recovered.stagedChunks, 0); assert.equal(recovered.stagedObjects, 0);
    assert.ok(recovered.usage.r2Deletes >= 1, "cold recovery must finish scheduled quarantine cleanup");
    const shallowRecovered = await responseJson(await fetch(`${server.origin}/_test/${shallowNamespace}/state`, { headers: headers() }));
    assert.equal(shallowRecovered.quarantineRows, 0, "shallow-only quarantine must be reclaimed after cold recovery");
    for (const item of restart) {
      const state = await responseJson(await fetch(`${server.origin}/_test/${item.namespace}/state`, { headers: headers() }));
      assert.notEqual(state.epoch, before);
      assert.equal(state.refs, 0);
      await canonical(server, item.namespace, item.result, item.row, item.expected);
      const after = await responseJson(await fetch(`${server.origin}/_test/${item.namespace}/state`, { headers: headers() }));
      assert.ok(after.usage.r2Gets > 0, "cold instance must read durable R2 data");
    }
    const body = randomBytes(CHUNK_BYTES + 1024 * 1024);
    const value = object(0, 3, body);
    const packed = pack(0, [entry(3, value.body)]);
    const fixture = { pack: packed, wire: wire(0, packed, value.id) };
    const namespace = `chunked-${randomUUID()}`;
    const result = await responseJson(await fetch(`${server.origin}/_test/${namespace}/intake`, { method: "POST", headers: headers(), body: chunked(fixture.wire, 65536), duplex: "half" }));
    assert.equal(result.packBytes, packed.length);
    assert.ok(result.usage.r2Puts >= 4, "multiple encoded and temporary canonical chunks must reach R2");
    assert.ok(result.usage.r2Deletes >= 2, "large temporary bodies are reclaimed after validation");
    assert.equal(result.refsUpdated, 0);
    assert.ok(result.caches.canonicalPeak <= 24 * 1024 * 1024);
    assert.ok(result.caches.representationPeak <= 8 * 1024 * 1024);
    assert.ok(result.caches.chunkPeak <= 16 * 1024 * 1024);
    const corrupted = Buffer.from(fixture.wire); corrupted[corrupted.length - 1] ^= 1;
    const rejected = await fetch(`${server.origin}/_test/${namespace}/intake`, { method: "POST", headers: headers(), body: chunked(corrupted, 65536), duplex: "half" });
    assert.equal(rejected.status, 400); await rejected.text();
    const shared = await responseJson(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
    assert.equal(shared.stagedChunks, 2, "failed owner must release claims while keeping both chunks owned by a verified stage");
    assert.equal(shared.stagedObjects, 1, "failed intake metadata must be reclaimed");
    const retained = await responseJson(await fetch(`${server.origin}/_test/${namespace}/object/${result.stage}?node=12`, { headers: headers() }));
    assert.equal(retained.oid, value.id.toString("hex"), "shared first chunk must remain readable after failed intake cleanup");

    const failedNamespace = `reclaim-${randomUUID()}`;
    const invalidCommit = object(0, 1, `tree ${"0".repeat(40)}\n\nmissing target\n`);
    const invalidPack = pack(0, [entry(3, value.body), entry(1, invalidCommit.body)]);
    const invalid = await fetch(`${server.origin}/_test/${failedNamespace}/intake`, { method: "POST", headers: headers(), body: chunked(wire(0, invalidPack, invalidCommit.id), 65536), duplex: "half" });
    assert.equal(invalid.status, 400); await invalid.text();
    const reclaimed = await responseJson(await fetch(`${server.origin}/_test/${failedNamespace}/state`, { headers: headers() }));
    assert.equal(reclaimed.stages[0].phase, "failed");
    assert.equal(reclaimed.stagedChunks, 0); assert.equal(reclaimed.stagedObjects, 0);
    assert.ok(reclaimed.usage.r2Deletes >= 4, "both encoded chunks and both private canonical chunks must be deleted");
  } catch (error) { error.message += `\n${server?.logs() ?? ""}`; throw error; }
  finally { await server?.stop(); await rm(persist, { recursive: true, force: true }); }
});
