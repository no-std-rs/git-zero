import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { project, packet, receiveReport, object, entry, pack, wire } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

const env = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" };
function git(args, input, version) {
  // Reference/client repositories are disposable protocol fixtures.
  const result = spawnSync("git", ["-c", "core.fsync=none", ...args], { input, env: { ...env, ...(version === undefined ? {} : { GIT_PROTOCOL: `version=${version}` }) }, timeout: 30000 });
  assert.equal(result.status, 0, result.stderr.toString()); return result.stdout;
}
function packBytes(bytes) {
  const chunks = []; let offset = 0;
  while (offset < bytes.length) {
    if (bytes.subarray(offset, offset + 4).toString() === "PACK") { chunks.push(bytes.subarray(offset)); break; }
    const size = Number.parseInt(bytes.subarray(offset, offset + 4).toString(), 16);
    assert.ok(Number.isInteger(size) && size >= 0); offset += 4;
    if (size < 4) continue;
    const data = bytes.subarray(offset, offset + size - 4); offset += size - 4;
    if (data[0] === 1) chunks.push(data.subarray(1));
  }
  return Buffer.concat(chunks);
}
function controls(format, version, wanted, have, filter) {
  const capability = `object-format=${format ? "sha256" : "sha1"}`;
  const lines = version === 2 ? [packet("command=fetch\n"), packet(capability + "\n"), Buffer.from("0001")] : [];
  lines.push(packet(`want ${wanted}${version === 2 ? "" : ` ${capability}${filter ? " filter" : ""}`}\n`));
  if (filter) lines.push(packet(`filter ${filter}\n`));
  if (version !== 2) lines.push(Buffer.from("0000"));
  lines.push(packet(`have ${have}\n`), packet("done\n"));
  if (version === 2) lines.push(Buffer.from("0000"));
  return Buffer.concat(lines);
}
function inventory(directory, format, bytes) {
  git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, directory]);
  git(["--git-dir", directory, "index-pack", "--stdin"], packBytes(bytes));
  git(["--git-dir", directory, "fsck", "--full", "--strict"]);
  return git(["--git-dir", directory, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]).toString().trim().split("\n").filter(Boolean).sort();
}

testRuntimes("{runtime} preserves explicitly wanted tree closures despite common commits", { timeout: 120000 }, async (serve) => {
  const directory = await mkdtemp(join(tmpdir(), "git-zero-wanted-tree-")); let server;
  try {
    server = await serve(join(directory, "state"));
    for (const format of [0, 1]) {
      const fixture = project(format), namespace = `wants-${format}`, commit = fixture.root.id.toString("hex"), tree = fixture.objects.find((o) => o.kind === 2).id.toString("hex"), blob = fixture.objects.find((o) => o.kind === 3).id.toString("hex"), source = join(directory, `source-${format}`);
      const tag = (target, name) => object(format, 4, `object ${target.id.toString("hex")}\ntype ${["", "commit", "tree", "blob", "tag"][target.kind]}\ntag ${name}\ntagger A <a@b> 0 +0000\n\nreference\n`);
      const treeTag = tag(fixture.objects.find((o) => o.kind === 2), "tree-tag"), blobTag = tag(fixture.objects.find((o) => o.kind === 3), "blob-tag"), commitTag = tag(fixture.root, "commit-tag"), nested = tag(treeTag, "nested-tag");
      const tagged = pack(format, [...fixture.objects, treeTag, blobTag, commitTag, nested].map((o) => entry(o.kind, o.body))), id = (o) => o.id.toString("hex");
      const scenarios = [
        ["tree", tree, null, [tree, blob]],
        ["filtered-tree", tree, "blob:none", [tree]],
        ["blob", blob, null, [blob]],
        ["filtered-blob", blob, "blob:none", [blob]],
        ["tree-tag", id(treeTag), null, [id(treeTag), tree, blob]],
        ["filtered-tree-tag", id(treeTag), "blob:none", [id(treeTag), tree]],
        ["blob-tag", id(blobTag), null, [id(blobTag), blob]],
        ["filtered-blob-tag", id(blobTag), "blob:none", [id(blobTag), blob]],
        ["commit-tag", id(commitTag), null, [id(commitTag)]],
        ["filtered-commit-tag", id(commitTag), "blob:none", [id(commitTag)]],
        ["nested-tree-tag", id(nested), null, [id(nested), id(treeTag), tree, blob]],
        ["filtered-nested-tree-tag", id(nested), "blob:none", [id(nested), id(treeTag), tree]],
      ];
      git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, source]);
      git(["--git-dir", source, "index-pack", "--stdin", "--strict"], tagged);
      git(["--git-dir", source, "update-ref", "refs/heads/main", commit]);
      git(["--git-dir", source, "config", "uploadpack.allowAnySHA1InWant", "true"]);
      git(["--git-dir", source, "config", "uploadpack.allowFilter", "true"]);
      let response = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(response.status, 200); await response.arrayBuffer();
      response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: wire(format, tagged, fixture.root.id, "refs/heads/main") }); assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs/);
      for (const version of [0, 1, 2]) for (const [label, wanted, filter, expected] of scenarios) {
        const body = controls(format, version, wanted, commit, filter), reference = git(["upload-pack", "--stateless-rpc", source], body, version), prefix = `${format}-${version}-${label}`;
        const native = inventory(join(directory, `native-${prefix}`), format, reference); assert.deepEqual(native, expected.sort());
        const fetched = await fetch(`${server.origin}/${namespace}.git/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}` }, body }); assert.equal(fetched.status, 200);
        assert.deepEqual(inventory(join(directory, `own-${prefix}`), format, Buffer.from(await fetched.arrayBuffer())), native, prefix);
      }
    }
  } finally { await server?.stop(); await rm(directory, { recursive: true, force: true }); }
});
