import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { project, object, entry, pack, wire, packet, receiveReport } from "./fixture.mjs";
import { headers, start } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

const env = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" };
function run(args, input, version) {
  return spawnSync("git", ["-c", "core.fsync=none", ...args], {
    input, env: { ...env, ...(version === undefined ? {} : { GIT_PROTOCOL: `version=${version}` }) }, timeout: 30000,
  });
}
function git(args, input, version) {
  const result = run(args, input, version); assert.equal(result.status, 0, result.stderr.toString()); return result.stdout;
}
function request(format, version, wanted, value, includeTags = false) {
  const capability = `object-format=${format ? "sha256" : "sha1"}`;
  const lines = version === 2 ? [packet("command=fetch\n"), packet(capability + "\n"), Buffer.from("0001")] : [];
  lines.push(packet(`want ${wanted}${version === 2 ? "" : ` ${capability} filter${includeTags ? " include-tag" : ""}`}\n`), packet(`filter object:type=${value}\n`));
  if (version === 2 && includeTags) lines.push(packet("include-tag\n"));
  if (version !== 2) lines.push(Buffer.from("0000"));
  lines.push(packet("done\n"));
  if (version === 2) lines.push(Buffer.from("0000"));
  return Buffer.concat(lines);
}
function inventory(directory, format, response) {
  let offset = 0; const chunks = [];
  while (offset < response.length) {
    if (response.subarray(offset, offset + 4).toString() === "PACK") { chunks.push(response.subarray(offset)); break; }
    assert.ok(offset + 4 <= response.length, "missing fetch pack");
    const size = Number.parseInt(response.subarray(offset, offset + 4).toString(), 16);
    assert.ok(Number.isInteger(size) && size >= 0); offset += 4;
    if (size < 4) continue;
    const data = response.subarray(offset, offset + size - 4); offset += size - 4;
    if (data[0] === 1) chunks.push(data.subarray(1));
  }
  const packed = Buffer.concat(chunks); assert.equal(packed.subarray(0, 4).toString(), "PACK");
  git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, directory]);
  git(["--git-dir", directory, "index-pack", "--stdin"], packed);
  return git(["--git-dir", directory, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)"]).toString().trim().split("\n").filter(Boolean).sort();
}

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

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} matches native object-type filters and explicit tagged wants`, { timeout: 120000 }, async () => {
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
      for (const version of [0, 1, 2]) for (const value of cases) for (const target of fixture.objects) for (const includeTags of [false, true]) {
        const wanted = target.id.toString("hex");
        const body = request(format, version, wanted, value, includeTags), native = run(["upload-pack", "--stateless-rpc", source], body, version), label = `${format}/${version}/${JSON.stringify(value)}/want-${target.kind}-${wanted}/tags-${includeTags}`;
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
