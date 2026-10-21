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
    const invalidNamespace = `invalid-format-${randomUUID()}`;
    const invalidFormat = await fetch(`${server.origin}/_test/${invalidNamespace}/initialize`, { method: "POST", headers: { ...headers(), "x-test-format": "2" } });
    assert.equal(invalidFormat.status, 400); await invalidFormat.arrayBuffer();
    await responseJson(await fetch(`${server.origin}/_test/${invalidNamespace}/initialize`, { method: "POST", headers: headers(1) }));
    const corrected = await responseJson(await fetch(`${server.origin}/_test/${invalidNamespace}/state`, { headers: headers() }));
    assert.equal(corrected.stages.length, 0);
    const emptyNamespace = `empty-receive-${randomUUID()}`;
    for (let attempt = 0; attempt < 70; attempt++) {
      const response = await fetch(`${server.origin}/${emptyNamespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: "0000" });
      assert.equal(response.status, 200); await response.arrayBuffer();
    }
    const emptyState = await responseJson(await fetch(`${server.origin}/_test/${emptyNamespace}/state`, { headers: headers() }));
    assert.equal(emptyState.stages.length, 0, "empty receives must not accumulate durable stage records");
    const batchNamespace = `batch-intake-${randomUUID()}`;
    const batches = Array.from({ length: 1025 }, (_, i) => object(0, 3, `bounded batch ${i}`));
    const batchPack = pack(0, batches.map((item) => entry(3, item.body)));
    const batchResult = await upload(server, batchNamespace, { wire: wire(0, batchPack, batches.at(-1).id) }, 0);
    assert.equal(batchResult.objects, batches.length);
    assert.ok(batchResult.usage.sqlCalls < batches.length / 2 + 100, `intake must batch complete records: ${batchResult.usage.sqlCalls} SQL calls`);
    const batchRows = []; let after = -1;
    for (;;) {
      const page = await responseJson(await fetch(`${server.origin}/_test/${batchNamespace}/stage/${batchResult.stage}?after=${after}`, { headers: headers() }));
      if (!page.objects.length) break;
      batchRows.push(...page.objects); after = page.objects.at(-1).node;
    }
    assert.equal(batchRows.length, batches.length, "the last partial batch must be flushed");
    for (const at of [0, 511, 1024]) await canonical(server, batchNamespace, batchResult, batchRows[at], batches[at]);
    const queueNamespace = `validation-queue-${randomUUID()}`;
    const prefix = Array.from({ length: 4097 }, (_, i) => object(0, 3, `completed prefix ${i}`));
    const base = prefix.at(-1);
    const pending = Array.from({ length: 1025 }, (_, i) => {
      const suffix = Buffer.from(` suffix ${i}`);
      const tag = object(0, 4, `object ${base.id.toString("hex")}\ntype blob\ntag pending-${i}\n\nqueued\n`);
      const delta = Buffer.concat([Buffer.from([base.body.length, base.body.length + suffix.length, 0x90, base.body.length, suffix.length]), suffix]);
      return [entry(4, tag.body), entry(7, delta, base.id)];
    });
    const queued = pack(0, [...prefix.map((item) => entry(3, item.body)), ...pending.flat()]);
    const queueResult = await upload(server, queueNamespace, { wire: wire(0, queued, base.id) }, 0);
    assert.equal(queueResult.objects, prefix.length + pending.length * 2);
    assert.ok(queueResult.usage.sqlRowsRead < queueResult.objects * 8, `validation must visit pending work instead of repeatedly scanning the completed prefix: ${queueResult.usage.sqlRowsRead} rows read`);
    const wideNamespace = `wide-tree-${randomUUID()}`;
    const leaf = object(0, 3, "wide tree payload");
    const wide = object(0, 2, Buffer.concat(Array.from({ length: 257 }, (_, i) => Buffer.concat([Buffer.from(`100644 file${String(i).padStart(3, "0")}\0`), leaf.id]))));
    const widePack = pack(0, [entry(3, leaf.body), entry(2, wide.body)]);
    const wideResult = await upload(server, wideNamespace, { wire: wire(0, widePack, wide.id) }, 0);
    assert.ok(wideResult.usage.sqlCalls < 180, `tree metadata must be batched: ${wideResult.usage.sqlCalls} SQL calls`);
    await canonical(server, wideNamespace, wideResult, await rowFor(server, wideNamespace, wideResult, wide.id), wide);
    const emptyDirectory = object(0, 2, "");
    const alias = object(0, 2, Buffer.concat([Buffer.from("100644 foo\0"), leaf.id, ...Array.from({ length: 32 }, (_, i) => Buffer.concat([Buffer.from(`100644 foo.${String(i).padStart(2, "0")}\0`), leaf.id])), Buffer.from("40000 foo\0"), emptyDirectory.id]));
    const aliasPack = pack(0, [entry(3, leaf.body), entry(2, emptyDirectory.body), entry(2, alias.body)]);
    const aliasNamespace = `wide-alias-${randomUUID()}`;
    const aliasResponse = await fetch(`${server.origin}/_test/${aliasNamespace}/intake`, { method: "POST", headers: headers(), body: wire(0, aliasPack, alias.id) });
    assert.equal(aliasResponse.status, 400); await aliasResponse.arrayBuffer();
    const aliasState = await responseJson(await fetch(`${server.origin}/_test/${aliasNamespace}/state`, { headers: headers() }));
    assert.deepEqual(aliasState.stages, [], "aliases separated across metadata batches must reject and reclaim");
    for (const format of [0, 1]) {
      for (const layout of ["mixed", "forward", "offsets"]) {
        const namespace = `delta-${format}-${layout}-${randomUUID()}`;
        const fixture = deltas(format, layout);
        const result = await upload(server, namespace, fixture, format);
        assert.equal(result.quarantine, true);
        assert.equal(result.refsUpdated, 0);
        assert.equal(result.objects, 3);
        assert.equal(result.queries, 2, "plain blob bytes authenticated by intake need no second reconstruction");
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
      const scanned = await responseJson(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      assert.equal(scanned.treeNameScratchRows, 0, "validated tree names must not duplicate immutable tree bodies in durable storage");
      const nullLink = object(format, 2, Buffer.concat([Buffer.from("160000 external\0"), Buffer.alloc(format ? 32 : 20)]));
      const target = object(format, 3, "one target cannot have two kinds");
      const mixed = object(format, 2, Buffer.concat([Buffer.from("100644 a\0"), target.id, Buffer.from("40000 b\0"), target.id]));
      const mixedKinds = { wire: wire(format, pack(format, [entry(3, target.body), entry(2, mixed.body)]), mixed.id) };
      for (const [label, invalid] of [["missing", project(format, true)], ["alias", project(format, false, true)], ["null-gitlink", { wire: wire(format, pack(format, [entry(2, nullLink.body)]), nullLink.id) }], ["mixed-kinds", mixedKinds]]) {
        const name = `${label}-${format}-${randomUUID()}`;
        const response = await fetch(`${server.origin}/_test/${name}/intake`, { method: "POST", headers: headers(format), body: invalid.wire });
        assert.equal(response.status, 400, await response.text());
        const state = await responseJson(await fetch(`${server.origin}/_test/${name}/state`, { headers: headers() }));
        assert.equal(state.refs, 0); assert.equal(state.objects, 0);
        assert.deepEqual(state.stages, [], "failed stages disappear only after their owned bytes and metadata are reclaimed");
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
    assert.deepEqual(shallowRecovered.stages, []);
    for (const item of restart) {
      const state = await responseJson(await fetch(`${server.origin}/_test/${item.namespace}/state`, { headers: headers() }));
      assert.notEqual(state.epoch, before);
      assert.equal(state.refs, 0);
      await canonical(server, item.namespace, item.result, item.row, item.expected);
      const after = await responseJson(await fetch(`${server.origin}/_test/${item.namespace}/state`, { headers: headers() }));
      assert.ok(after.usage.r2Gets > 0, "cold instance must read durable R2 data");
    }
    const body = randomBytes(9 * 1024 * 1024);
    const value = object(0, 3, body);
    const enlarged = object(0, 3, Buffer.concat([body, Buffer.from("!")]));
    const size = (value) => { const bytes = []; do { const byte = value & 127; value = Math.floor(value / 128); bytes.push(byte | (value ? 128 : 0)); } while (value); return Buffer.from(bytes); };
    const instructions = Buffer.concat([size(body.length), size(body.length + 1), Buffer.from([0xf0, body.length & 255, (body.length >> 8) & 255, body.length >> 16, 1, 33])]);
    const plainEntry = entry(3, value.body), deltaEntry = entry(7, instructions, value.id);
    const packed = pack(0, [plainEntry, deltaEntry]);
    const fixture = { pack: packed, wire: wire(0, packed, enlarged.id) };
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
    assert.equal(shared.stagedChunks, Math.ceil(packed.length / CHUNK_BYTES), "failed owner must release claims while keeping every chunk owned by a verified stage");
    assert.equal(shared.stagedObjects, 2, "failed intake metadata must be reclaimed");
    const retained = await responseJson(await fetch(`${server.origin}/_test/${namespace}/object/${result.stage}?node=12`, { headers: headers() }));
    assert.equal(retained.oid, value.id.toString("hex"), "shared first chunk must remain readable after failed intake cleanup");

    const failedNamespace = `reclaim-${randomUUID()}`;
    const invalidCommit = object(0, 1, `tree ${"1".repeat(40)}\n\nmissing target\n`);
    const invalidPack = pack(0, [plainEntry, deltaEntry, entry(1, invalidCommit.body)]);
    const invalid = await fetch(`${server.origin}/_test/${failedNamespace}/intake`, { method: "POST", headers: headers(), body: chunked(wire(0, invalidPack, invalidCommit.id), 65536), duplex: "half" });
    assert.equal(invalid.status, 400); await invalid.text();
    const reclaimed = await responseJson(await fetch(`${server.origin}/_test/${failedNamespace}/state`, { headers: headers() }));
    assert.deepEqual(reclaimed.stages, []);
    assert.equal(reclaimed.stagedChunks, 0); assert.equal(reclaimed.stagedObjects, 0);
    const reclaimedChunks = Math.ceil(invalidPack.length / CHUNK_BYTES) + Math.ceil(body.length / CHUNK_BYTES) + Math.ceil(enlarged.body.length / CHUNK_BYTES);
    assert.ok(reclaimed.usage.r2Deletes >= reclaimedChunks, "every encoded and temporary canonical chunk must be deleted");
  } catch (error) { error.message += `\n${server?.logs() ?? ""}`; throw error; }
  finally { await server?.stop(); await rm(persist, { recursive: true, force: true }); }
});
