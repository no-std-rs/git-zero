import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { project, packet, receiveReport } from "./fixture.mjs";
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
function request(format, version, wanted, value) {
  const capability = `object-format=${format ? "sha256" : "sha1"}`;
  const lines = version === 2 ? [packet("command=fetch\n"), packet(capability + "\n"), Buffer.from("0001")] : [];
  lines.push(packet(`want ${wanted}${version === 2 ? "" : ` ${capability} filter`}\n`), packet(`filter blob:limit=${value}\n`));
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

const cases = [
  ["14", 14n], ["016", 14n], ["017", 15n], ["0xe", 14n], ["0Xf", 15n],
  ["1k", 1024n], ["1K", 1024n], ["1M", 1048576n], ["1G", 1073741824n],
  ["+14", 14n], [" +017", 15n], ["\t14", 14n], ["\v14", 14n], ["\f14", 14n],
  ["+0x10K", 16384n], ["18446744073709551615", 18446744073709551615n],
  ["0xffffffffffffffff", 18446744073709551615n], ["01777777777777777777777", 18446744073709551615n],
  ["08", null], ["0x", null], ["++1", null], ["-0", null], ["1K ", null],
  ["18446744073709551616", null], ["18014398509481984K", null], ["0x10000000000000000", null],
];

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} matches native blob-filter number syntax and exact thresholds`, { timeout: 120000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-filter-numbers-")); let server;
  try {
    server = await serve(join(root, "state"));
    for (const format of [0, 1]) {
      const fixture = project(format), namespace = `numbers-${format}`, source = join(root, `source-${format}`), wanted = fixture.root.id.toString("hex");
      git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, source]);
      git(["--git-dir", source, "index-pack", "--stdin", "--strict"], fixture.pack);
      git(["--git-dir", source, "update-ref", "refs/heads/main", wanted]);
      git(["--git-dir", source, "config", "uploadpack.allowFilter", "true"]);
      let response = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(response.status, 200); await response.arrayBuffer();
      response = await fetch(`${server.origin}/${namespace}.git/git-receive-pack`, { method: "POST", headers: headers(), body: fixture.wire }); assert.equal(response.status, 200); assert.match(receiveReport(await response.arrayBuffer()), /ok refs/);
      for (const version of [0, 1, 2]) for (const [index, [value, limit]] of cases.entries()) {
        const body = request(format, version, wanted, value), native = run(["upload-pack", "--stateless-rpc", source], body, version), label = `${format}/${version}/${JSON.stringify(value)}`;
        const actual = await fetch(`${server.origin}/${namespace}.git/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request", "git-protocol": `version=${version}` }, body });
        const bytes = Buffer.from(await actual.arrayBuffer());
        if (limit === null) { assert.notEqual(native.status, 0, label); assert.equal(actual.status, 400, label); continue; }
        assert.equal(native.status, 0, `${label}: ${native.stderr}`); assert.equal(actual.status, 200, label);
        const expected = fixture.objects.filter((o) => o.kind !== 3 || BigInt(o.body.length) < limit).map((o) => o.id.toString("hex")).sort();
        const prefix = `${format}-${version}-${index}`;
        assert.deepEqual(inventory(join(root, `native-${prefix}`), format, native.stdout), expected, label);
        assert.deepEqual(inventory(join(root, `own-${prefix}`), format, bytes), expected, label);
      }
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
