import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { packet } from "./fixture.mjs";
import { headers } from "./wrangler.mjs";

function git(args, input) {
  const result = spawnSync("git", ["-c", "core.fsync=none", ...args], {
    input, timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_PROTOCOL: "version=2" },
  });
  assert.equal(result.status, 0, result.stderr.toString()); return result.stdout;
}
function command(format, args) {
  return Buffer.concat([packet("command=ls-refs\n"), packet(`object-format=${format ? "sha256" : "sha1"}\n`), Buffer.from("0001"), ...args.map((arg) => packet(arg + "\n")), Buffer.from("0000")]);
}
function controls(format, length) {
  const header = command(format, []).subarray(0, -4), parts = [header]; let left = length - header.length - 4;
  while (left) {
    let size = Math.min(left, 65520);
    if (left > size && left - size < 17) size -= 17;
    parts.push(packet("ref-prefix " + "a".repeat(size - 16) + "\n")); left -= size;
  }
  parts.push(Buffer.from("0000")); return Buffer.concat(parts);
}

testRuntimes("{runtime} matches native unborn ls-refs and accepts gzip framing at the decoded control limit", { timeout: 120000 }, async (serve) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-lsrefs-coding-")); let server;
  try {
    server = await serve(join(root, "state"));
    for (const format of [0, 1]) {
      const source = join(root, `source-${format}`), namespace = `coding-${format}`;
      git(["init", "--bare", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`, source]);
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const url = `${server.origin}/${namespace}.git/git-upload-pack`, requestHeaders = { ...headers(), "content-type": "application/x-git-upload-pack-request", "git-protocol": "version=2" };
      const native = (body) => git(["upload-pack", "--stateless-rpc", source], body);
      for (const args of [[], ["unborn"], ["symrefs"], ["unborn", "symrefs"], ["unborn", "ref-prefix HEAD"], ["unborn", "symrefs", "ref-prefix missing"]]) {
        const body = command(format, args), expected = native(body), response = await fetch(url, { method: "POST", headers: requestHeaders, body });
        assert.equal(response.status, 200); assert.deepEqual(Buffer.from(await response.arrayBuffer()), expected);
      }
      const limit = 4 * 1024 * 1024;
      for (const length of [limit, limit + 1]) {
        const decoded = controls(format, length), encoded = gzipSync(decoded, { level: 0 }), expected = native(decoded);
        assert.equal(decoded.length, length); assert.ok(encoded.length > length, "stored deflate blocks exercise framing beyond the decoded limit");
        for (const coding of ["identity", "gzip", "x-gzip"]) {
          const response = await fetch(url, { method: "POST", headers: { ...requestHeaders, "content-encoding": coding }, body: coding === "identity" ? decoded : encoded });
          assert.equal(response.status, length === limit ? 200 : 400, `${format}/${coding}/${length}`);
          const bytes = Buffer.from(await response.arrayBuffer());
          if (length === limit) assert.deepEqual(bytes, expected);
        }
      }
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
