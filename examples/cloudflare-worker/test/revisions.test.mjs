import assert from "node:assert/strict";
import { test } from "node:test";
import { createRepositoryWorker, importRepository, repositoryRequest } from "../src/repository.mjs";
import { createWorker } from "../src/worker.mjs";
import { environmentRevisions, revisionPolicy } from "../src/revision-policy.mjs";
import { withServer } from "./http.mjs";
import { consume, execute, fetchBody, module, oracle, withFixture } from "./revision-fixture.mjs";

const limits = Object.freeze({ maxExclusions: 8, graphReadLimit: 1048576 });
const headers = (version) => new Headers({ "Git-Protocol": `version=${version}`, "Content-Type": "application/x-git-upload-pack-request" });

for (const format of ["sha1", "sha256"]) {
  test(`Worker time/exclusion history matches Git http-backend, v0/v1/v2, ${format}`, { timeout: 180000 }, async () => {
    await withFixture(format, async ({ root, repository }) => {
      const worker = createRepositoryWorker(module, (request) => ({
        repository, target: new URL(request.url).pathname.slice("/repo.git/".length) + new URL(request.url).search,
        revisionLimits: limits,
      }));
      await withServer((request) => worker.fetch(request), async (origin) => {
        const result = await execute("python3", [oracle, "compare", "--root", root, "--zero-url", origin + "/repo.git"], { timeout: 150000 });
        assert.match(result.stdout, /v0 revision/); assert.match(result.stdout, /v1 revision/); assert.match(result.stdout, /v2 revision/);
      });
    });
  });

  test(`revision ABI limits, cancellation, independent instances and strict reset, ${format}`, async () => {
    await withFixture(format, async ({ repository, ids }) => {
      const { exports } = new WebAssembly.Instance(module, {});
      const memory = exports.memory.buffer;
      assert.ok(memory.byteLength <= 24 * 65536);
      importRepository(exports, repository);
      for (const version of [0, 1, 2]) {
        const stage = (args = ["deepen-since 1700000200"]) => repositoryRequest(exports, "git-upload-pack", fetchBody(repository, version, args), headers(version));
        let length = repositoryRequest(exports, "info/refs?service=git-upload-pack", Buffer.alloc(0), headers(version));
        assert.equal(exports.stream_start_repository_revisions(length, 0, 0, 64, 8, 1048576), 3);
        const advertisement = consume(exports);
        if (version === 2) assert.ok(advertisement.includes(Buffer.from("fetch=shallow")));
        else {
          assert.ok(advertisement.includes(Buffer.from("deepen-since")));
          assert.ok(advertisement.includes(Buffer.from("deepen-not")));
        }
        length = repositoryRequest(exports, "info/refs?service=git-upload-pack", Buffer.alloc(0), headers(version));
        assert.equal(exports.stream_start_repository(length, 0), 3);
        const strict = consume(exports);
        assert.ok(!strict.includes(Buffer.from("deepen-since")));
        assert.ok(!strict.includes(Buffer.from("deepen-not")));
        length = stage();
        assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), 3);
        const expected = consume(exports, 4096);
        assert.ok(expected.includes(Buffer.from(`shallow ${ids.B}`)));
        for (let attempt = 0; attempt < 16; attempt++) {
          length = stage();
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), 3);
          assert.deepEqual(consume(exports, 1), expected);
          length = stage();
          assert.equal(exports.stream_start_repository(length, 1), -2);
          assert.equal(exports.stream_read(1), -2);
          length = stage();
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 0), -1);
          assert.equal(exports.stream_read(1), -2);
          length = stage(["deepen-not cutoff", "deepen-not cutoff"]);
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 1, 1048576), -1);
          assert.equal(exports.stream_read(1), -2);
          length = stage(["deepen-not cutoff", "deepen-not cutoff"]);
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 2, 1048576), 3);
          assert.ok(consume(exports).length > 0);
          length = stage();
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 0, 8, 1048576), -1);
          assert.equal(exports.stream_read(1), -2);
          length = stage();
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 9, 1048576), -2);
          assert.equal(exports.stream_read(1), -2);
          length = stage();
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048577), -2);
          assert.equal(exports.stream_read(1), -2);
          length = stage();
          assert.equal(exports.stream_start_repository_revisions(length, 1, 7, 64, 8, 1048576), -2);
          assert.equal(exports.stream_read(1), -2);
          assert.equal(exports.memory.buffer, memory);
        }
        for (const args of [
          ["deepen 1", "deepen-since 0"], ["deepen-since invalid"], ["deepen-since 08"],
          ["deepen-not ambiguous"], ["deepen-not missing"], ["deepen-not main^"],
          ["deepen-since 0", "filter tree:1"], ["deepen-since 0", "filter object:type=commit"],
        ]) {
          length = stage(args);
          assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), -2, args.join(", "));
          assert.equal(exports.stream_read(1), -2);
        }
        length = stage();
        assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), 3);
        assert.equal(exports.stream_read(1), 1);
        exports.stream_cancel();
        assert.equal(exports.stream_read(1), -2);
        length = stage();
        assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), 3);
        const second = new WebAssembly.Instance(module, {}).exports;
        importRepository(second, repository);
        const other = repositoryRequest(second, "git-upload-pack", fetchBody(repository, version, ["deepen-not cutoff"]), headers(version));
        assert.equal(second.stream_start_repository_revisions(other, 1, 0, 64, 8, 1048576), 3);
        second.stream_cancel();
        assert.deepEqual(consume(exports), expected);
        assert.equal(exports.memory.buffer, memory);
      }
    });
  });

  test(`revision capability and promised-object grant remain independent, ${format}`, async () => {
    await withFixture(format, async ({ repository, ids }) => {
      const exports = new WebAssembly.Instance(module, {}).exports;
      importRepository(exports, repository);
      // The fixture exposes the shared blob through a tag; use the tree to test
      // a reachable object that is neither an advertised nor a peeled endpoint.
      const tree = fetchBody(repository, 2, [], { id: ids.tree });
      let length = repositoryRequest(exports, "git-upload-pack", tree, headers(2));
      assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), -2);
      length = repositoryRequest(exports, "git-upload-pack", tree, headers(2));
      assert.equal(exports.stream_start_repository_revisions(length, 1, 1, 64, 8, 1048576), 3);
      assert.ok(consume(exports).length > 0);
      length = repositoryRequest(exports, "git-upload-pack", tree, headers(2));
      assert.equal(exports.stream_start_repository_revisions(length, 1, 0, 64, 8, 1048576), -2);
      assert.equal(exports.stream_read(1), -2);
    });
  });
}

test("trusted revision policy validates exact fixed budgets and required ABI exports", () => {
  assert.equal(revisionPolicy(module, undefined), null);
  assert.equal(revisionPolicy(module, null), null);
  assert.deepEqual(revisionPolicy(module, limits, "stream_start_repository_revisions"), limits);
  assert.ok(Object.isFrozen(revisionPolicy(module, limits)));
  for (const value of [true, "enabled", [], {}, { ...limits, maxExclusions: -1 }, { ...limits, maxExclusions: 9 },
    { ...limits, maxExclusions: 0.5 }, { ...limits, graphReadLimit: 1048577 }, { ...limits, graphReadLimit: NaN },
    { ...limits, graphReadLimit: "10" }]) {
    assert.throws(() => revisionPolicy(module, value), TypeError);
  }
  assert.throws(() => revisionPolicy(module, limits, "missing_export"), /missing_export/);
  assert.equal(environmentRevisions(module, {}), null);
  assert.equal(environmentRevisions(module, { GIT_REVISION_HISTORY: "disabled" }), null);
  assert.deepEqual(environmentRevisions(module, { GIT_REVISION_HISTORY: "enabled" }), limits);
  assert.deepEqual(environmentRevisions(module, { GIT_REVISION_HISTORY: "enabled", GIT_MAX_EXCLUSIONS: "0", GIT_GRAPH_READ_LIMIT: "0" }), { maxExclusions: 0, graphReadLimit: 0 });
  assert.throws(() => environmentRevisions(module, { GIT_REVISION_HISTORY: "yes" }), TypeError);
});

test("metadata snapshot host only advertises revision capabilities with explicit environment opt-in", async () => {
  const worker = createWorker(module);
  for (const layout of ["flat", "tree"]) {
    const request = new Request("https://worker.invalid/");
    const info = await (await worker.fetch(request, { SNAPSHOT_LAYOUT: layout })).json();
    const url = info.clone_url + "/info/refs?service=git-upload-pack";
    const strict = await worker.fetch(new Request(url));
    assert.ok(!(await strict.text()).includes("deepen-since"));
    const enabled = await worker.fetch(new Request(url), { GIT_REVISION_HISTORY: "enabled" });
    assert.ok((await enabled.text()).includes("deepen-since"));
    const reset = await worker.fetch(new Request(url));
    assert.ok(!(await reset.text()).includes("deepen-since"));
    assert.equal((await worker.fetch(new Request(url), { GIT_REVISION_HISTORY: "bad" })).status, 503);
  }
});

test("trusted history and publication scalars are copied before an awaited request body", async () => {
  await withFixture("sha1", async ({ repository }) => {
    let release;
    const ready = new Promise((resolve) => { release = resolve; });
    const revisionLimits = { maxExclusions: 8, graphReadLimit: 1048576 };
    const selected = { repository, target: "git-upload-pack", revisionLimits, publication: "packed-only" };
    const worker = createRepositoryWorker(module, () => { release(); return selected; });
    let finishBody;
    const body = new ReadableStream({ start(controller) { finishBody = () => {
      controller.enqueue(fetchBody(repository, 2, ["deepen-since 1700000200"])); controller.close();
    }; } });
    const request = new Request("https://worker.invalid/repo.git/git-upload-pack", {
      method: "POST", headers: headers(2), body, duplex: "half",
    });
    const pending = worker.fetch(request);
    await ready;
    // The loader resolved before the host can finish reading the paused body.
    await new Promise((resolve) => setImmediate(resolve));
    revisionLimits.maxExclusions = 0; revisionLimits.graphReadLimit = 0; selected.publication = "invalid";
    finishBody();
    const response = await pending;
    assert.equal(response.status, 200); assert.ok((await response.arrayBuffer()).byteLength > 0);
    // A subsequent request snapshots the new invalid configuration and fails
    // before it can read a body or prepare any repository output.
    const invalid = await worker.fetch(new Request("https://worker.invalid/repo.git/HEAD"));
    assert.equal(invalid.status, 503);
  });
});

test("request headers cannot enable history or override a host's zero graph budget", async () => {
  await withFixture("sha1", async ({ repository }) => {
    const body = fetchBody(repository, 2, ["deepen-since 1700000200"]);
    const request = () => new Request("https://worker.invalid/repo.git/git-upload-pack", {
      method: "POST", body,
      headers: { ...Object.fromEntries(headers(2)), "GIT_REVISION_HISTORY": "enabled", "X-Graph-Read-Limit": "1048576" },
    });
    const strict = createRepositoryWorker(module, () => ({ repository, target: "git-upload-pack" }));
    assert.equal((await strict.fetch(request())).status, 400);
    const limited = createRepositoryWorker(module, () => ({ repository, target: "git-upload-pack", revisionLimits: { maxExclusions: 8, graphReadLimit: 0 } }));
    assert.equal((await limited.fetch(request())).status, 413);
    const granted = createRepositoryWorker(module, () => ({ repository, target: "git-upload-pack", revisionLimits: limits }));
    const result = await granted.fetch(request());
    assert.equal(result.status, 200); await result.arrayBuffer();
  });
});
