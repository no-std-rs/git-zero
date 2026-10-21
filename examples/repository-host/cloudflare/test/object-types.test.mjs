import { combinations } from "./combinations.mjs";
import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { project, object, entry, pack, wire, receiveReport } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

import { run, git, request, inventory } from "./upload-oracle.mjs";

const cases = ["commit", "tree", "blob", "tag", "", "Blob", "tree ", "invalid"];
function graph(format) {
  const fixture = project(format), objects = [...fixture.objects];
  const tag = (target, name) => {
    const value = object(format, 4, `object ${target.id.toString("hex")}\ntype ${["", "commit", "tree", "blob", "tag"][target.kind]}\ntag ${name}\ntagger A <a@b> 0 +0000\n\n${name}\n`);
    objects.push(value); return value;
  };
  const blobTag = tag(objects[0], "blob"), treeTag = tag(objects[1], "tree"), commitTag = tag(fixture.root, "commit"), nested = tag(commitTag, "nested");
  const packed = pack(format, objects.map((o) => entry(o.kind, o.body)));
  return { objects, pack: packed, wire: wire(format, packed, nested.id), root: nested };
}

testRuntimes("{runtime} matches native object-type filters and explicit tagged wants", { timeout: 120000 }, async (serve) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-object-types-")); let server;
  try {
    server = await serve(join(root, "state"));
    for (const format of [0, 1]) {
      const fixture = graph(format), namespace = `types-${format}`, source = join(root, `source-${format}`), wanted = fixture.root.id.toString("hex");
      git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, source]);
      git(["--git-dir", source, "index-pack", "--stdin", "--strict"], fixture.pack);
      git(["--git-dir", source, "update-ref", "refs/tags/value", wanted]);
      git(["--git-dir", source, "config", "uploadpack.allowFilter", "true"]);
      git(["--git-dir", source, "config", "uploadpack.allowAnySHA1InWant", "true"]);
      let response = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(response.status, 200); await response.arrayBuffer();
      response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: fixture.wire }); assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs/);
      let index = 0;
      for (const [version, value, target, includeTags] of combinations([0, 1, 2], cases, fixture.objects, [false, true])) {
        const wanted = target.id.toString("hex");
        const body = request(format, version, wanted, `object:type=${value}`, { includeTags }), native = run(["upload-pack", "--stateless-rpc", source], body, version), label = `${format}/${version}/${JSON.stringify(value)}/want-${target.kind}-${wanted}/tags-${includeTags}`;
        const actual = await fetch(`${server.origin}/${namespace}.git/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}` }, body });
        const bytes = Buffer.from(await actual.arrayBuffer());
        if (!["commit", "tree", "blob", "tag"].includes(value)) { assert.notEqual(native.status, 0, label); assert.equal(actual.status, 400, label); continue; }
        assert.equal(native.status, 0, `${label}: ${native.stderr}`); assert.equal(actual.status, 200, label);
        const prefix = `${format}-${version}-${index++}`;
        const expected = inventory(join(root, `native-${prefix}`), format, native.stdout);
        assert.ok(expected.includes(wanted), label);
        assert.deepEqual(inventory(join(root, `own-${prefix}`), format, bytes), expected, label);
      }
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
