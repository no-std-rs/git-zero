// Literal upload requests go unchanged to native Git and the own HTTP server.
// Native Git is a test oracle; production hosting never imports this module.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { packet } from "./fixture.mjs";

const env = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" };
export function run(args, input, version) {
  return spawnSync("git", ["-c", "core.fsync=none", ...args], {
    input, env: { ...env, ...(version === undefined ? {} : { GIT_PROTOCOL: `version=${version}` }) }, timeout: 30000,
  });
}
export function git(args, input, version) {
  const result = run(args, input, version); assert.equal(result.status, 0, result.stderr.toString()); return result.stdout;
}
export function request(format, version, wanted, filter, { includeTags = false, have = null, depth = null } = {}) {
  const capability = `object-format=${format ? "sha256" : "sha1"}`;
  const lines = version === 2 ? [packet("command=fetch\n"), packet(capability + "\n"), Buffer.from("0001")] : [];
  lines.push(packet(`want ${wanted}${version === 2 ? "" : ` ${capability} filter${includeTags ? " include-tag" : ""}`}\n`));
  if (depth !== null) lines.push(packet(`deepen ${depth}\n`));
  lines.push(packet(`filter ${filter}\n`));
  if (version === 2 && includeTags) lines.push(packet("include-tag\n"));
  if (version !== 2) lines.push(Buffer.from("0000"));
  if (have !== null) lines.push(packet(`have ${have}\n`));
  lines.push(packet("done\n"));
  if (version === 2) lines.push(Buffer.from("0000"));
  return Buffer.concat(lines);
}
export function inventory(directory, format, response) {
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
