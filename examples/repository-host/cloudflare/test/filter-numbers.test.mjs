import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { project, receiveReport } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

import { run, git, request, inventory } from "./upload-oracle.mjs";

const cases = [
  ["14", 14n], ["016", 14n], ["017", 15n], ["0xe", 14n], ["0Xf", 15n],
  ["1k", 1024n], ["1K", 1024n], ["1M", 1048576n], ["1G", 1073741824n],
  ["+14", 14n], [" +017", 15n], ["\t14", 14n], ["\v14", 14n], ["\f14", 14n],
  ["+0x10K", 16384n], ["18446744073709551615", 18446744073709551615n],
  ["0xffffffffffffffff", 18446744073709551615n], ["01777777777777777777777", 18446744073709551615n],
  ["08", null], ["0x", null], ["++1", null], ["-0", null], ["1K ", null],
  ["18446744073709551616", null], ["18014398509481984K", null], ["0x10000000000000000", null],
];

testRuntimes("{runtime} matches native blob-filter number syntax and exact thresholds", { timeout: 120000 }, async (serve) => {
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
        const body = request(format, version, wanted, `blob:limit=${value}`), native = run(["upload-pack", "--stateless-rpc", source], body, version), label = `${format}/${version}/${JSON.stringify(value)}`;
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
