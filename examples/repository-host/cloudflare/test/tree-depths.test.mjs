import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { object, entry, pack, wire, receiveReport } from "./fixture.mjs";
import { run, git, request, inventory } from "./upload-oracle.mjs";
import { headers } from "./wrangler.mjs";

function graph(format) {
  const objects = [], add = (kind, body) => { const o = object(format, kind, body); objects.push(o); return o; };
  const tree = (entries) => add(2, Buffer.concat(entries.map(([name, target]) => Buffer.concat([Buffer.from(`${target.kind === 2 ? "40000" : "100644"} ${name}\0`), target.id]))));
  const commit = (root, parent, name) => add(1, `tree ${root.id.toString("hex")}\n${parent ? `parent ${parent.id.toString("hex")}\n` : ""}author A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\n${name}\n`);
  const tag = (target, name) => add(4, `object ${target.id.toString("hex")}\ntype ${["", "commit", "tree", "blob", "tag"][target.kind]}\ntag ${name}\ntagger A <a@b> 0 +0000\n\n${name}\n`);
  const blob = add(3, "shared leaf\n"), shared = tree([["leaf", blob]]), deep = tree([["inner", shared]]);
  const baseBlob = add(3, "base\n"), base = commit(tree([["base", baseBlob]]), null, "base");
  const rootBlob = add(3, "root\n"), main = commit(tree([["a-deep", deep], ["root", rootBlob], ["z-shallow", shared]]), base, "main");
  const blobTag = tag(blob, "blob"), treeTag = tag(shared, "tree"), commitTag = tag(main, "main"), nested = tag(commitTag, "nested");
  return { objects, main, base, shared, blob, treeTag, nested, tags: [blobTag, treeTag, commitTag, nested], packed: pack(format, objects.map((o) => entry(o.kind, o.body))) };
}
function profiles(fixture) {
  return [
    { wanted: fixture.main }, { wanted: fixture.shared }, { wanted: fixture.blob },
    { wanted: fixture.nested }, { wanted: fixture.treeTag },
    { wanted: fixture.main, have: fixture.base }, { wanted: fixture.shared, have: fixture.main },
    { wanted: fixture.main, depth: 1 }, { wanted: fixture.main, includeTags: true },
  ];
}

testRuntimes("{runtime} matches native tree-depth filters across shared paths and history", { timeout: 120000 }, async (serve) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-tree-depths-")); let server;
  try {
    server = await serve(join(root, "state"));
    for (const format of [0, 1]) {
      const fixture = graph(format), namespace = `trees-${format}`, source = join(root, `source-${format}`);
      git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, source]);
      git(["--git-dir", source, "index-pack", "--stdin", "--strict"], fixture.packed);
      git(["--git-dir", source, "config", "uploadpack.allowFilter", "true"]);
      git(["--git-dir", source, "config", "uploadpack.allowAnySHA1InWant", "true"]);
      let response = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(response.status, 200); await response.arrayBuffer();
      for (const [index, target] of [fixture.main, ...fixture.tags].entries()) {
        const name = index ? `refs/tags/value-${index}` : "refs/heads/main";
        git(["--git-dir", source, "update-ref", name, target.id.toString("hex")]);
        response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, index ? pack(format, []) : fixture.packed, target.id, name) });
        assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs/);
      }
      const cases = profiles(fixture).flatMap((profile) => ["0", "1", "2", "3"].map((limit) => ({ ...profile, limit })));
      for (const limit of ["016", "+0x2", "1K", "18446744073709551615", "08", "-1", "++1", "18446744073709551616"]) cases.push({ wanted: fixture.main, limit });
      let index = 0;
      for (const version of [0, 1, 2]) for (const profile of cases) {
        const wanted = profile.wanted.id.toString("hex"), opts = { ...profile, have: profile.have?.id.toString("hex") ?? null };
        const body = request(format, version, wanted, `tree:${profile.limit}`, opts), label = `${format}/${version}/${index}/${profile.limit}`;
        const native = run(["upload-pack", "--stateless-rpc", source], body, version);
        const actual = await fetch(`${server.origin}/${namespace}.git/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}` }, body });
        const bytes = Buffer.from(await actual.arrayBuffer());
        if (["08", "-1", "++1", "18446744073709551616"].includes(profile.limit)) { assert.notEqual(native.status, 0, label); assert.equal(actual.status, 400, label); continue; }
        assert.equal(native.status, 0, `${label}: ${native.stderr}`); assert.equal(actual.status, 200, label);
        const prefix = `${format}-${version}-${index++}`, expected = inventory(join(root, `native-${prefix}`), format, native.stdout);
        assert.ok(expected.includes(wanted), label);
        assert.deepEqual(inventory(join(root, `own-${prefix}`), format, bytes), expected, label);
      }
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
