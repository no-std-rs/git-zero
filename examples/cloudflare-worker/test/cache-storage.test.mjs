import assert from "node:assert/strict";
import { test } from "node:test";
import { createPackCache, decodePackRecord, encodePackRecord, MAX_PACK_BYTES, packCachePolicy } from "../src/pack-cache.mjs";
import { backend, cache, deferred, generated, key, module, prepare } from "./cache-fixture.mjs";
import { fixtures } from "./repository-fixture.mjs";

const source = prepare(fixtures[0]);
const identity = key(source.exports);
const raw = generated(source.exports);

for (const kind of ["kv", "r2"]) {
  test(`${kind} cache awaits publication, verifies every hit and preserves private versioned keys`, () => checkAwaitedPublication(kind));

  test(`${kind} same-key cold misses coordinate, then every response rechecks storage independently`, () => checkColdMissCoordination(kind));

  test(`${kind} failed reads and publication preserve existing records and release cold-miss state`, () => checkStorageFailureRecovery(kind));

  test(`${kind} expired and evicted records regenerate, new isolates retain valid hits`, () => checkExpiryAndReopen(kind));

  test(`${kind} corrupt envelopes fail closed without generation or insertion`, () => checkCorruptEnvelopes(kind));

  test(`${kind} oversize records cancel bounded reads and exact byte limits accept complete packs`, () => checkBoundedRecords(kind));
}

test("cache records validate schema, selected identity, lengths, expiry and every checksum byte", async () => {
  const good = await encodePackRecord(identity, raw, 200);
  assert.equal(good.length, raw.length + 100);
  assert.equal(Buffer.compare(Buffer.from(await decodePackRecord(identity, good, raw.length, 199)), raw), 0);
  assert.equal(await decodePackRecord(identity, good, raw.length, 200), null);
  for (const index of [0, 7, 8, 15, 16, 63, 64, 67, 68, good.length - 33, good.length - 1]) {
    const altered = good.slice();
    altered[index] ^= 1;
    await assert.rejects(decodePackRecord(identity, altered, raw.length, 100), /integrity|size/);
  }
  const other = prepare(fixtures[1]);
  await assert.rejects(decodePackRecord(key(other.exports), good, raw.length, 100), /integrity/);
  await assert.rejects(decodePackRecord(identity, good, raw.length - 1, 100), /size/);
  await assert.rejects(encodePackRecord(identity, new Uint8Array(MAX_PACK_BYTES + 1), 200), /exceeds/);
  await assert.rejects(encodePackRecord("bogus", raw, 200), /key/);
  await assert.rejects(encodePackRecord(identity, raw, 0), /expiry/);
});

test("cache policy rejects invalid limits, implicit namespaces, malformed keys and missing ABI", async () => {
  const binding = backend();
  const base = { backend: "kv", binding, namespace: "private" };
  for (const options of [
    { backend: "disk" }, { binding: {} }, { namespace: undefined }, { namespace: "../public" }, { namespace: "" },
    { maxBytes: 31 }, { maxBytes: MAX_PACK_BYTES + 1 }, { maxBytes: 42.5 },
    { ttlSeconds: 59 }, { ttlSeconds: 31536001 }, { maxInflight: 0 }, { maxInflight: 1025 }, { now: 100 },
  ]) assert.throws(() => createPackCache({ ...base, ...options }), TypeError);
  const policy = createPackCache(base);
  for (const invalid of ["", "x".repeat(96), identity.toUpperCase(), `00${identity.slice(2)}`, `${identity.slice(0, 10)}03${identity.slice(12)}`]) {
    await assert.rejects(policy.resolve(invalid, () => raw, () => {}), /key/);
  }
  assert.equal(binding.operations.length, 0);
  assert.equal(packCachePolicy(module, null), null);
  assert.equal(packCachePolicy(module, policy), policy);
  assert.throws(() => packCachePolicy(module, {}), TypeError);
  const empty = new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]));
  assert.throws(() => packCachePolicy(empty, policy), /requires Wasm export/);
  assert.ok(Object.isFrozen(policy));
  assert.ok(Object.isFrozen(policy.statistics()));
});

test("producer admission is bounded across keys and authentication failures never publish", async () => {
  const { binding, policy } = cache("kv", { maxInflight: 1 });
  binding.getGate = deferred();
  const first = policy.resolve(identity, () => raw, () => {});
  await until(() => binding.operations.length === 1);
  const second = prepare(fixtures[1]);
  await assert.rejects(policy.resolve(key(second.exports), () => raw, () => {}), /producer limit/);
  assert.equal(binding.operations.length, 1);
  binding.getGate.resolve();
  await first;
  binding.records.clear();
  await assert.rejects(policy.resolve(identity, () => raw, () => { throw new Error("wrong closure"); }), /wrong closure/);
  assert.equal(binding.records.size, 0);
  assert.equal(policy.statistics().inflight, 0);
  assert.equal(await policy.resolve(identity, () => raw, () => {}), "miss");
});

test("cancelled storage waits and followers release state without cancelling other responses", async () => {
  const { binding, policy } = cache();
  binding.getGate = deferred();
  const firstController = new AbortController();
  const first = policy.resolve(identity, () => raw, () => {}, firstController.signal);
  await until(() => binding.operations.length === 1);
  const followerController = new AbortController();
  const follower = policy.resolve(identity, () => raw, () => {}, followerController.signal);
  followerController.abort();
  await assert.rejects(follower, /cancelled/);
  assert.equal(policy.statistics().inflight, 1);
  firstController.abort();
  await assert.rejects(first, /cancelled/);
  assert.equal(policy.statistics().inflight, 0);
  binding.getGate.resolve();
  assert.equal(await policy.resolve(identity, () => raw, () => {}), "miss");
  const already = new AbortController();
  already.abort();
  const operations = binding.operations.length;
  await assert.rejects(policy.resolve(identity, () => raw, () => {}, already.signal), /cancelled/);
  assert.equal(binding.operations.length, operations);
});

async function until(predicate) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail("cache did not reach expected operation");
}

test("same-key waiter admission is bounded and cancellation releases a slot immediately", async () => {
  const { binding, policy } = cache("kv", { maxWaiters: 1 });
  binding.getGate = deferred();
  const first = policy.resolve(identity, () => raw, () => {});
  await until(() => binding.operations.length === 1);
  const controller = new AbortController();
  const waiting = policy.resolve(identity, () => raw, () => {}, controller.signal);
  assert.equal(policy.statistics().waiters, 1);
  await assert.rejects(policy.resolve(identity, () => raw, () => {}), /waiter limit/);
  assert.equal(binding.operations.length, 1);
  controller.abort();
  await assert.rejects(waiting, /cancelled/);
  assert.equal(policy.statistics().waiters, 0);
  const replacement = policy.resolve(identity, () => raw, () => {});
  binding.getGate.resolve();
  assert.equal(await first, "miss");
  assert.equal(await replacement, "hit");
  assert.equal(policy.statistics().waiters, 0);
});

test("abort cancels an outstanding record reader and late get results release their streams", async () => {
  for (const kind of ["kv", "r2"]) {
    for (const phase of ["get", "body"]) {
      const gate = deferred();
      let cancelled = 0, started = false;
      const binding = {
        async get() {
          started = true;
          if (phase === "get") await gate.promise;
          const body = new ReadableStream({ cancel() { cancelled++; } });
          return kind === "kv" ? body : { body, size: 100 };
        },
        async put() { assert.fail("cancelled read must not publish"); },
      };
      const policy = createPackCache({ backend: kind, binding, namespace: "cancel-reader" });
      const controller = new AbortController();
      const pending = policy.resolve(identity, () => { assert.fail("cancelled read must not generate"); }, () => {}, controller.signal);
      await until(() => started);
      // Let a completed get install the bounded reader before aborting it.
      await new Promise((resolve) => setImmediate(resolve));
      controller.abort();
      await assert.rejects(pending, /cancelled/);
      gate.resolve();
      await until(() => cancelled === 1);
      assert.equal(policy.statistics().inflight, 0);
    }
  }
});

async function checkAwaitedPublication(kind) {
  const { binding, policy } = cache(kind);
  binding.putGate = deferred();
  let generates = 0, authenticates = 0, completed = false;
  const first = policy.resolve(identity, () => { generates++; return raw; }, (pack) => {
    authenticates++; assert.equal(Buffer.compare(Buffer.from(pack), raw), 0);
  }).then((result) => { completed = true; return result; });
  await until(() => binding.operations.some((value) => value.operation === "put"));
  assert.equal(completed, false);
  assert.equal(binding.records.size, 0);
  assert.equal(authenticates, 1);
  binding.putGate.resolve();
  assert.equal(await first, "miss");
  assert.equal(binding.records.size, 1);
  const entry = [...binding.records.keys()][0];
  assert.equal(entry, `selected-packs/v1/test-private/${identity}`);
  assert.equal(await policy.resolve(identity, () => { throw new Error("hit generated"); }, (pack) => {
    authenticates++; assert.equal(Buffer.compare(Buffer.from(pack), raw), 0);
  }), "hit");
  assert.equal(generates, 1);
  assert.equal(authenticates, 2);
  assert.deepEqual(policy.statistics(), {
    hits: 1, misses: 1, waits: 0, writes: 1, failures: 0, producers: 0, peakProducers: 1, inflight: 0, waiters: 0,
  });
  if (kind === "kv") {
    assert.equal(binding.operations[0].type, "stream");
    assert.deepEqual(binding.operations.find((value) => value.operation === "put").options, { expirationTtl: 3600 });
  } else {
    assert.equal(binding.operations[0].type, undefined);
    assert.deepEqual(binding.operations.find((value) => value.operation === "put").options,
      { httpMetadata: { contentType: "application/octet-stream" } });
  }
}

async function checkColdMissCoordination(kind) {
  const { binding, policy } = cache(kind);
  binding.getGate = deferred();
  let generates = 0, authenticates = 0;
  const generate = () => { generates++; return raw; };
  const authenticate = (pack) => { authenticates++; assert.equal(Buffer.compare(Buffer.from(pack), raw), 0); };
  const first = policy.resolve(identity, generate, authenticate);
  await until(() => binding.operations.length === 1);
  const followers = Array.from({ length: 8 }, () => policy.resolve(identity, generate, authenticate));
  assert.equal(binding.operations.length, 1);
  binding.getGate.resolve();
  assert.deepEqual(await Promise.all([first, ...followers]), ["miss", ...Array(8).fill("hit")]);
  assert.equal(generates, 1);
  assert.equal(authenticates, 9);
  assert.equal(policy.statistics().writes, 1);
  assert.equal(policy.statistics().inflight, 0);
  assert.equal(policy.statistics().peakProducers, 1);
  assert.equal(binding.operations.filter((value) => value.operation === "get").length, 9);
}

async function checkStorageFailureRecovery(kind) {
  const { binding, policy } = cache(kind);
  await policy.resolve(identity, () => raw, () => {});
  const saved = [...binding.records.values()][0].slice();
  binding.readError = new Error("backend offline");
  await assert.rejects(policy.resolve(identity, () => { throw new Error("must not generate"); }, () => {}), /read failed/);
  assert.equal(policy.statistics().inflight, 0);
  assert.equal(Buffer.compare(Buffer.from([...binding.records.values()][0]), Buffer.from(saved)), 0);
  binding.readError = null;
  binding.nextRead = await encodePackRecord(identity, raw, 1);
  binding.writeError = new Error("write failed");
  await assert.rejects(policy.resolve(identity, () => raw, () => {}), /publication failed/);
  assert.equal(policy.statistics().inflight, 0);
  assert.equal(Buffer.compare(Buffer.from([...binding.records.values()][0]), Buffer.from(saved)), 0);
  binding.writeError = null;
  assert.equal(await policy.resolve(identity, () => { throw new Error("preserved hit"); }, () => {}), "hit");
}

async function checkExpiryAndReopen(kind) {
  let now = 100;
  const { binding, policy } = cache(kind, { now: () => now, ttlSeconds: 60 });
  let generations = 0;
  const generate = () => { generations++; return raw; };
  assert.equal(await policy.resolve(identity, generate, () => {}), "miss");
  now = 159;
  assert.equal(await policy.resolve(identity, generate, () => {}), "hit");
  now = 160;
  assert.equal(await policy.resolve(identity, generate, () => {}), "miss", "expiry is inclusive");
  const reloaded = createPackCache({ backend: kind, binding, namespace: "test-private", now: () => now });
  assert.equal(await reloaded.resolve(identity, generate, () => {}), "hit");
  binding.records.clear();
  assert.equal(await reloaded.resolve(identity, generate, () => {}), "miss");
  assert.equal(generations, 3);
}

async function checkCorruptEnvelopes(kind) {
  const { binding, policy } = cache(kind, { now: () => 100 });
  const good = await encodePackRecord(identity, raw, 200);
  const variants = [good.slice(0, -1), new Uint8Array([...good, 0]), good.slice()];
  variants[2][70] ^= 1;
  for (const record of variants) {
    binding.nextRead = record;
    let generated = false, attached = false;
    await assert.rejects(policy.resolve(identity, () => { generated = true; return raw; }, () => { attached = true; }), /validation|integrity/);
    assert.equal(generated, false);
    assert.equal(attached, false);
    assert.equal(binding.records.size, 0);
    assert.equal(policy.statistics().inflight, 0);
  }
}

async function checkBoundedRecords(kind) {
  const { binding, policy } = cache(kind, { maxBytes: raw.length });
  assert.equal(await policy.resolve(identity, () => raw, () => {}), "miss");
  assert.equal(await policy.resolve(identity, () => { throw new Error("unexpected miss"); }, () => {}), "hit");
  const small = createPackCache({ backend: kind, binding, namespace: "test-private", maxBytes: raw.length - 1 });
  await assert.rejects(small.resolve(identity, () => raw, () => {}), /read|record/);
  binding.records.clear();
  await assert.rejects(small.resolve(identity, () => raw, () => {}), /exceeds/);
  assert.equal(binding.records.size, 0);
  let cancelled = false;
  const bad = { get: async () => {
    const body = new ReadableStream({
      start(controller) { controller.enqueue(new Uint8Array(MAX_PACK_BYTES + 101)); },
      cancel() { cancelled = true; },
    });
    return kind === "kv" ? body : { size: MAX_PACK_BYTES + 101, body };
  }, put: async () => { throw new Error("must not write"); } };
  const bounded = createPackCache({ backend: kind, binding: bad, namespace: "bounded" });
  await assert.rejects(bounded.resolve(identity, () => raw, () => {}), /read|record/);
  assert.equal(cancelled, true);
}
