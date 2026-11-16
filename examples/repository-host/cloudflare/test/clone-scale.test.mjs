import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { generated, git } from "./browse-scale-fixture.mjs";
import { startStandalone } from "./standalone.mjs";
import { receive } from "./browse-fixture.mjs";
import { packet, pack, receiveReport } from "./fixture.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";

test("25k default-packed commits: full gateway clone has bounded progress intervals and compiled SQL templates", { timeout: 300000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-clone-scale-")); let worker;
  try {
    const fixture = await generated(root, 25000, 0, true), packed = git(fixture.dir, ["pack-objects", "--stdout", "--all", "--delta-base-offset", "--threads=1"]);
    worker = await startStandalone(join(root, "state"), { authMode: "gateway", grantKey: GRANT_KEY, packCache: false });
    const call = (route, method = "GET", body) => fetch(`${worker.origin}/scale.git/${route}`, { method, headers: { "x-git-zero-grant": mint("scale", "admin"), "content-type": route.startsWith("git-") ? `application/x-${route}-request` : "application/json" }, body });
    const created = await call("api/v1/repository", "PUT", "{}"); assert.equal(created.status, 201);
    const pushed = await call("git-receive-pack", "POST", receive(0, fixture.refs.map((ref) => ({ name: ref.name, new: ref.oid, old: "0".repeat(40) })), packed));
    if (pushed.status !== 200) assert.fail(await pushed.text()); assert.doesNotMatch(receiveReport(await pushed.arrayBuffer()), /ng /);
    for (const next of ["0".repeat(40), fixture.tip]) {
      const rewritten = await call("git-receive-pack", "POST", receive(0, [{ name: "refs/heads/rewritten", old: next === fixture.tip ? "0".repeat(40) : fixture.old, new: next }], pack(0, [])));
      assert.equal(rewritten.status, 200); assert.doesNotMatch(receiveReport(await rewritten.arrayBuffer()), /ng /);
    }
    git(fixture.dir, ["update-ref", "refs/heads/rewritten", fixture.tip]);
    const tips = [...new Set(fixture.refs.map((ref) => ref.name === "refs/heads/rewritten" ? fixture.tip : ref.oid))];
    const controls = Buffer.concat([...tips.map((oid, i) => packet(`want ${oid}${i ? "" : " side-band-64k ofs-delta"}\n`)), Buffer.from("0000"), packet("done\n")]);
    const started = performance.now(), response = await call("git-upload-pack", "POST", controls), firstByteMs = performance.now() - started;
    if (response.status !== 200) assert.fail(await response.text());
    const wire = Buffer.from(await response.arrayBuffer()); assert.ok(wire.includes(Buffer.from("PACK")));
    const stateResponse = await fetch(`${worker.origin}/_test/scale/state`, { headers: { "x-git-zero-grant": mint("scale", "admin") } }), state = await stateResponse.json();
    assert.ok(state.preparation.objects >= 150000); assert.ok(state.preparation.compilations < 100); assert.ok(state.preparation.hits > 500000);
    const chunks = []; let offset = 0;
    while (offset < wire.length) {
      const length = Number.parseInt(wire.subarray(offset, offset + 4).toString(), 16); offset += 4;
      assert.ok(Number.isInteger(length)); if (length < 4) continue;
      const part = wire.subarray(offset, offset + length - 4); offset += length - 4;
      if (part[0] === 1) chunks.push(part.subarray(1));
    }
    const cloned = join(root, "cloned.git"); git(fixture.dir, ["init", "--bare", cloned]);
    git(fixture.dir, ["--git-dir", cloned, "index-pack", "--stdin", "--strict"], Buffer.concat(chunks));
    const actual = git(fixture.dir, ["--git-dir", cloned, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]).toString().trim().split("\n").sort();
    const expected = git(fixture.dir, ["rev-list", "--objects", "--no-object-names", "--all"]).toString().trim().split("\n").sort();
    assert.deepEqual(actual, expected); assert.equal(actual.includes(fixture.old), false);
    // This measures template compilation+SQLite together on the actual runtime;
    // the separate microbenchmark reports rewriting without storage/I/O.
    t.diagnostic(JSON.stringify({ fixtureCommits: 25000, fixtureObjects: state.objects, firstByteMs, wireBytes: wire.length, ...state.preparation }));
  } finally { await worker?.stop(); await rm(root, { recursive: true, force: true }); }
});
