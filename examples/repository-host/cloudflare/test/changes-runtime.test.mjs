import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { project, pack, receiveReport } from "./fixture.mjs";
import { receive } from "./browse-fixture.mjs";
import { headers } from "./wrangler.mjs";

testRuntimes("{runtime} retains paged publication records through cold restarts in both formats", { timeout: 60000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-changes-")); let server;
  try {
    server = await serve(directory);
    for (const format of [0, 1]) {
      const ns = `changes-${format}`, fixture = project(format), tip = fixture.root.id.toString("hex"), zero = "0".repeat(tip.length);
      const url = (route) => `${server.origin}/${ns}.git/${route}`;
      const created = await fetch(url("api/v1/repository"), { method: "PUT", headers: { ...headers(), "content-type": "application/json" }, body: JSON.stringify({ objectFormat: format ? "sha256" : "sha1" }) }); assert.equal(created.status, 201); await created.arrayBuffer();
      const updates = Array.from({ length: 9 }, (_, i) => ({ name: i ? `refs/tags/t${i}` : "refs/heads/main", old: zero, new: tip }));
      const pushed = await fetch(url("git-receive-pack"), { method: "POST", headers: headers(), body: receive(format, updates, fixture.pack) }); assert.match(receiveReport(await pushed.arrayBuffer()), /ok refs\/heads\/main/);
      const first = await (await fetch(url("api/v1/changes?limit=2"), { headers: headers() })).json(); assert.equal(first.publications[0].actor, ""); assert.equal(first.publications[0].complete, false);
      await server.stop(); server = await serve(directory);
      const result = [...first.publications[0].updates]; let cursor = first.nextCursor, completed;
      while (cursor) { const response = await fetch(url(`api/v1/changes?cursor=${cursor}&limit=3`), { headers: headers() }); assert.equal(response.status, 200); const page = await response.json(); result.push(...page.publications.flatMap((p) => p.updates)); cursor = page.nextCursor; completed = page.completedThrough; }
      assert.equal(completed, 1); assert.deepEqual(result.map(({ name, oldId, newId }) => ({ name, old: oldId, new: newId })), updates);
      const deleted = await fetch(url("git-receive-pack"), { method: "POST", headers: headers(), body: receive(format, [{ name: "refs/heads/main", old: tip, new: zero }], pack(format, [])) }); assert.match(receiveReport(await deleted.arrayBuffer()), /ok refs\/heads\/main/);
      const next = await (await fetch(url("api/v1/changes?after=1"), { headers: headers() })).json(); assert.equal(next.completedThrough, 2); assert.equal(next.publications[0].updates[0].newId, zero);
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
