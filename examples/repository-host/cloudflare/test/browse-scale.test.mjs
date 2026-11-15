import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { spawnSync } from "node:child_process";
import { mkdtemp, mkdir, rm, readFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { OwnRepository } from "./host.mjs";
import { sqliteContext } from "./sqlite.mjs";
import { GRANT_KEY, mint } from "./grant-fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { pack, receiveReport } from "./fixture.mjs";
import { createRepositoryHost } from "../src/host.mjs";
const codec = await WebAssembly.compile(await readFile(new URL("../build/git_zero_host_wasm.wasm", import.meta.url)));
function git(dir, args, input) {
  const result = spawnSync("git", args, { cwd: dir, input, maxBuffer: 256 * 1024 * 1024, timeout: 120000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`); return result.stdout;
}
async function generated(root, count, format) {
  const dir = join(root, `${count}-${format}`); await mkdir(dir);
  git(dir, ["init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`]);
  const commands = [];
  for (let i = 1; i <= count; i++) {
    const body = `analysis revision ${i}\n`;
    commands.push(`blob\nmark :${i * 2}\ndata ${Buffer.byteLength(body)}\n${body}commit refs/heads/main\nmark :${i * 2 + 1}\nauthor Author <a@example.test> ${i} +0000\ncommitter Committer <c@example.test> ${i} +0000\ndata ${String(i).length + 8}\ncommit ${i}\nM 100644 :${i * 2} src/analysis.py\n\n`);
  }
  // A distinct branch is published and then rewritten, retaining its old bytes.
  commands.push(`reset refs/heads/other\nfrom :${Math.floor(count / 2) * 2 + 1}\n\nreset refs/tags/release\nfrom :${count * 2 + 1}\n\nblob\nmark :${count * 2 + 2}\ndata 7\nsecret\ncommit refs/heads/rewritten\nmark :${count * 2 + 3}\ncommitter Old <o@example.test> 1 +0000\ndata 9\nold fork\nfrom :3\nM 100644 :${count * 2 + 2} secret\n\ndone\n`);
  git(dir, ["fast-import", "--quiet", "--done"], commands.join(""));
  const id = (name) => git(dir, ["rev-parse", name]).toString().trim();
  return { dir, tip: id("refs/heads/main"), old: id("refs/heads/rewritten"), secret: id("refs/heads/rewritten:secret"), refs: git(dir, ["for-each-ref", "--format=%(refname) %(objectname)"]).toString().trim().split("\n").map((line) => { const [name, oid] = line.split(" "); return { name, oid }; }) };
}
for (const format of [0, 1]) test(`ref-rooted reads have constant SQL cost at 25000 real Git commits format=${format}`, { timeout: 240000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-browse-scale-")), costs = [];
  try {
    for (const count of [1000, 25000]) {
      const fixture = await generated(root, count, format), db = new DatabaseSync(":memory:"), ctx = sqliteContext(db), objects = new Map();
      let statements = 0; const exec = ctx.storage.sql.exec;
      ctx.storage.sql.exec = (...args) => { statements++; return exec(...args); };
      const BLOBS = { async get(key) { const data = objects.get(key); return data ? { size: data.length, async arrayBuffer() { return data.slice().buffer; } } : null; }, async put(key, data) { objects.set(key, new Uint8Array(data).slice()); }, async delete(keys) { for (const key of keys) objects.delete(key); } };
      const host = new OwnRepository(ctx, { AUTH_MODE: "gateway", GRANT_KEY, BLOBS });
      const call = (route, method = "GET", body, level = "read") => host.fetch(new Request(`http://host/scale.git/${route}`, { method, headers: { "x-git-zero-namespace": "scale", "x-git-zero-grant": mint("scale", level), "content-type": route === "git-receive-pack" ? "application/x-git-receive-pack-request" : "application/json" }, ...(body === undefined ? {} : { body }) }));
      async function push(updates, packed) { const response = await call("git-receive-pack", "POST", receive(format, updates, packed), "write"); assert.equal(response.status, 200, await response.clone().text()); assert.doesNotMatch(receiveReport(await response.arrayBuffer()), /ng /); await host.queue; }
      try {
        assert.equal((await call("api/v1/repository", "PUT", JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }), "admin")).status, 201);
        await push(fixture.refs.map((ref) => ({ ...ref, old: "0".repeat(format ? 64 : 40), new: ref.oid })), git(fixture.dir, ["pack-objects", "--stdout", "--all", "--window=0", "--depth=0"]));
        await push([{ name: "refs/heads/rewritten", old: fixture.old, new: "0".repeat(format ? 64 : 40) }], pack(format, []));
        await push([{ name: "refs/heads/rewritten", old: "0".repeat(format ? 64 : 40), new: fixture.tip }], pack(format, []));
        git(fixture.dir, ["update-ref", "refs/heads/rewritten", fixture.tip]);
        const objectCount = db.prepare("SELECT object_count n FROM repository").get().n;
        assert.ok(objectCount >= count * 4);
        const measured = {};
        for (const route of ["commit?rev=HEAD", "history?rev=main&limit=20", "tree?rev=release&path=src", "blob?rev=main&path=src/analysis.py"]) {
          const before = statements, changes = db.prepare("SELECT total_changes() n").get().n;
          const response = await call(`api/v1/${route}`); assert.equal(response.status, 200, await response.clone().text());
          const value = route.startsWith("blob") ? Buffer.from(await response.arrayBuffer()) : await response.json(); await host.queue;
          measured[route] = { statements: statements - before, rowWrites: db.prepare("SELECT total_changes() n").get().n - changes };
          assert.equal(measured[route].rowWrites, 0, route);
          assert.ok(measured[route].statements < 300, JSON.stringify(measured));
          if (route.startsWith("commit")) assert.equal(value.oid, fixture.tip);
          if (route.startsWith("history")) assert.deepEqual(value.commits.map((c) => c.oid), git(fixture.dir, ["rev-list", "--first-parent", "--max-count=20", "main"]).toString().trim().split("\n"));
          if (route.startsWith("tree")) assert.equal(value.entries[0].oid, git(fixture.dir, ["rev-parse", "main:src/analysis.py"]).toString().trim());
          if (route.startsWith("blob")) assert.deepEqual(value, git(fixture.dir, ["cat-file", "blob", "main:src/analysis.py"]));
        }
        costs.push(measured); t.diagnostic(JSON.stringify({ commits: count, objects: objectCount, format, measured }));
        // A raw proof advances incrementally; no old published fork can leak.
        const indexBefore = statements, indexWrites = db.prepare("SELECT total_changes() n").get().n;
        let response = await call(`api/v1/commit?rev=oid:${fixture.old}`), attempts = 1;
        assert.ok(statements - indexBefore < 2000, "one proof request has a bounded SQL batch");
        assert.ok(db.prepare("SELECT total_changes() n").get().n - indexWrites < 1000, "one proof request has bounded writes");
        assert.equal(response.status, 503); assert.match((await response.json()).error, /index is being built/); assert.equal(response.headers.get("retry-after"), "1");
        const firstProgress = db.prepare("SELECT objects FROM browse_index").get().objects;
        response = await call(`api/v1/blob?oid=${fixture.secret}`); assert.equal(response.status, 503);
        assert.ok(db.prepare("SELECT objects FROM browse_index").get().objects > firstProgress);
        if (count === 1000) {
          while (response.status === 503 && attempts++ < 100) response = await call(`api/v1/commit?rev=oid:${fixture.old}`);
          assert.equal(response.status, 404); assert.ok(attempts > 2);
          const before = statements, changes = db.prepare("SELECT total_changes() n").get().n;
          assert.equal((await call(`api/v1/commit?rev=oid:${fixture.old}`)).status, 404);
          assert.equal(db.prepare("SELECT total_changes() n").get().n, changes);
          assert.ok(statements - before < 30, "completed generation proof is reused");
          assert.equal((await call(`api/v1/blob?oid=${fixture.secret}`)).status, 404);
        }
        await host.queue;
      } finally { db.close(); }
    }
    assert.deepEqual(costs[1], costs[0], "SQL statement and row-write counts must not depend on history size");
  } finally { await rm(root, { recursive: true, force: true }); }
});
test("embedders can raise read budgets, with documented hard ceilings", () => {
  const options = { codec, bindings: { repositories: "R", objects: "B", grantKey: "K" }, authMode: "gateway", routePrefix: "", keyPrefix: "" };
  assert.doesNotThrow(() => createRepositoryHost({ ...options, readLimits: { visibleObjects: 100000000, edges: 1000000000, pageSize: 1000, blobBytes: 64 * 1024 * 1024 } }));
  assert.throws(() => createRepositoryHost({ ...options, readLimits: { visibleObjects: 100000001 } }));
});
