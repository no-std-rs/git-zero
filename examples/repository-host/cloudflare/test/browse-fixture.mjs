import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, writeFile, chmod, symlink } from "node:fs/promises";
import { join } from "node:path";
import { packet } from "./fixture.mjs";
export function git(directory, args, input) {
  const result = spawnSync("git", ["-c", "user.name=Author Δ", "-c", "user.email=author@example.test", ...args], { cwd: directory, input, timeout: 30000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  assert.equal(result.status, 0, `${args.join(" ")}\n${result.stderr}`); return result.stdout;
}
export async function source(root, format) {
  const directory = join(root, `source-${format}`); await mkdir(directory);
  git(directory, ["init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`]);
  await mkdir(join(directory, "src"));
  await writeFile(join(directory, "src", "analysis.py"), "# analysis framework\n".repeat(2000));
  await writeFile(join(directory, "payload.html"), "<script>globalThis.executed = true</script>\n");
  await writeFile(join(directory, "empty"), "");
  await writeFile(join(directory, "run"), "#!/bin/sh\ntrue\n"); await chmod(join(directory, "run"), 0o755);
  await symlink("src/analysis.py", join(directory, "link"));
  git(directory, ["add", "."]);
  git(directory, ["update-index", "--add", "--cacheinfo", `160000,${"1".repeat(format ? 64 : 40)},external`]);
  git(directory, ["commit", "-m", "initial subject", "-m", "first body α\nsecond body line"]);
  const first = git(directory, ["rev-parse", "HEAD"]).toString().trim();
  const secret = git(directory, ["rev-parse", "HEAD:src/analysis.py"]).toString().trim();
  git(directory, ["tag", "v1"]);
  await writeFile(join(directory, "src", "analysis.py"), "# analysis framework\n".repeat(1999) + "print('new')\n");
  git(directory, ["add", "src/analysis.py"]);
  git(directory, ["commit", "-m", "second subject", "-m", "second body"]);
  const tip = git(directory, ["rev-parse", "HEAD"]).toString().trim();
  git(directory, ["tag", "-a", "release", "-m", "release annotation"]);
  git(directory, ["tag", "-a", "nested", "release", "-m", "nested annotation"]);
  const emptyTree = git(directory, ["mktree"], "").toString().trim(); git(directory, ["tag", "empty-tree", emptyTree]);
  const blob = git(directory, ["rev-parse", "HEAD:payload.html"]).toString().trim();
  const wide = Array.from({ length: 1800 }, (_, index) => `100644 blob ${blob}\tfile-${String(index).padStart(5, "0")}-${"x".repeat(40)}\0`).join("");
  const wideTree = git(directory, ["mktree", "-z"], wide).toString().trim(); git(directory, ["tag", "wide-tree", wideTree]);
  const rawTree = git(directory, ["mktree", "-z"], Buffer.concat([Buffer.from(`100644 blob ${blob}\t`), Buffer.from([255, 0])])).toString().trim(); git(directory, ["tag", "raw-tree", rawTree]);
  const tree = git(directory, ["rev-parse", "HEAD^{tree}"]).toString().trim();
  const longCommit = git(directory, ["hash-object", "-t", "commit", "-w", "--stdin"], `tree ${tree}\nauthor A <a@b> 1 +0000\ncommitter B <b@c> 2 -0130\n\nlong subject\n\n${"body\n".repeat(20000)}`).toString().trim(); git(directory, ["tag", "long-message", longCommit]);
  const upper = git(directory, ["hash-object", "-t", "commit", "-w", "--stdin"], `tree ${tree.toUpperCase()}\nparent ${tip.toUpperCase()}\nauthor A <a@b> 1 +0000\ncommitter B <b@c> 2 -0130\n\nuppercase links\n`).toString().trim(); git(directory, ["tag", "upper-links", upper]);
  const trailing = git(directory, ["hash-object", "--literally", "-t", "commit", "-w", "--stdin"], `tree ${tree}\nparent ${tip}\nauthor A <a@b> 1 +0000\ncommitter B <b@c> 2 -0130\nparent ${first}\ntree ${emptyTree}\n\nlate structural lookalikes\n`).toString().trim(); git(directory, ["tag", "late-headers", trailing]);
  git(directory, ["checkout", "--orphan", "other"]); git(directory, ["rm", "-rf", "."]);
  await writeFile(join(directory, "different"), "unrelated root\n"); git(directory, ["add", "."]); git(directory, ["commit", "-m", "other root"]);
  const other = git(directory, ["rev-parse", "HEAD"]).toString().trim(); git(directory, ["checkout", "main"]);
  return { directory, first, tip, other, secret, emptyTree, format };
}
export function refs(fixture) {
  return git(fixture.directory, ["for-each-ref", "--format=%(refname) %(objectname)"]).toString().trim().split("\n").filter(Boolean).map((line) => { const at = line.indexOf(" "); return { name: line.slice(0, at), oid: line.slice(at + 1) }; });
}
export function receive(format, updates, packed) {
  const caps = `report-status-v2 delete-refs atomic side-band-64k object-format=${format ? "sha256" : "sha1"}`;
  return Buffer.concat([...updates.map((update, index) => packet(`${update.old} ${update.new} ${update.name}${index ? "" : `\0${caps}`}\n`)), Buffer.from("0000"), ...(updates.every((update) => /^0+$/.test(update.new)) ? [] : [packed])]);
}
export async function compare(fixture, get) {
  const { directory, tip } = fixture;
  const expectedRefs = refs(fixture), actualRefs = []; let cursor;
  do { const page = await get(`refs?limit=2${cursor ? `&cursor=${cursor}` : ""}`); actualRefs.push(...page.refs.map(({ name, oid }) => ({ name, oid }))); cursor = page.nextCursor; assert.equal(page.head.oid, tip); } while (cursor);
  assert.deepEqual(actualRefs, expectedRefs);
  const expectedTree = git(directory, ["ls-tree", "-l", "-z", "HEAD"]).toString().split("\0").filter(Boolean).map((line) => {
    const [prefix, name] = line.split("\t"), [mode, type, oid, size] = prefix.trim().split(/\s+/);
    return { name, mode, oid, size: type === "blob" ? Number(size) : null, kind: mode === "160000" ? "submodule" : mode === "120000" ? "symlink" : type };
  });
  const entries = []; cursor = null;
  do { const page = await get(`tree?limit=2${cursor ? `&cursor=${cursor}` : ""}`); entries.push(...page.entries.map(({ name, mode, oid, size, kind }) => ({ name, mode, oid, size, kind }))); cursor = page.nextCursor; } while (cursor);
  assert.deepEqual(entries, expectedTree);
  assert.equal((await get("tree?rev=refs/tags/empty-tree")).entries.length, 0);
  const nested = await get("tree?rev=nested&path=src"); assert.deepEqual(nested.entries.map((e) => e.name), ["analysis.py"]);
  const wide = await get("tree?rev=wide-tree&limit=100"); assert.equal(wide.entries.length, 100); assert.ok(wide.nextCursor);
  assert.deepEqual((await get("tree?rev=raw-tree")).entries.map(({ name, name64 }) => ({ name, name64 })), [{ name: null, name64: "_w" }]);
  const long = await get("commit?rev=long-message"); assert.equal(long.message.truncated, true); assert.equal(long.message.subject, "long subject"); assert.equal(Buffer.from(long.message.raw64, "base64url").length, 32768);
  const upper = await get("history?rev=upper-links&limit=3");
  assert.deepEqual(upper.commits.map((commit) => commit.oid), git(directory, ["rev-list", "--first-parent", "upper-links"]).toString().trim().split("\n"));
  assert.equal(upper.commits[0].tree, git(directory, ["rev-parse", "HEAD^{tree}"]).toString().trim()); assert.deepEqual(upper.commits[0].parents, [tip]);
  const late = await get("commit?rev=late-headers");
  assert.equal(late.tree, treeId(directory));
  assert.deepEqual(late.parents, git(directory, ["show", "-s", "--format=%P", "late-headers"]).toString().trim().split(" "));
  assert.deepEqual((await get("history?rev=late-headers&limit=3")).commits.map((c) => c.oid), git(directory, ["rev-list", "--first-parent", "--max-count=3", "late-headers"]).toString().trim().split("\n"));
  const commits = []; cursor = null;
  do { const page = await get(`history?limit=1${cursor ? `&cursor=${cursor}` : ""}`); commits.push(...page.commits); cursor = page.nextCursor; } while (cursor);
  assert.deepEqual(commits.map((entry) => entry.oid), git(directory, ["rev-list", "--first-parent", "HEAD"]).toString().trim().split("\n"));
  for (const commit of commits) {
    const expected = git(directory, ["show", "-s", "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%s%x00%b", commit.oid]).toString().trimEnd().split("\0");
    assert.deepEqual(commit.parents, expected[1] ? expected[1].split(" ") : []);
    assert.deepEqual([commit.author.name, commit.author.email, commit.author.time, commit.committer.name, commit.committer.email, commit.committer.time, commit.message.subject, commit.message.body.trimEnd()], expected.slice(2));
    const single = await get(`commit?rev=${commit.oid}`); assert.deepEqual(single, commit);
  }
  return { entries, commits };
}

function treeId(directory) { return git(directory, ["rev-parse", "HEAD^{tree}"]).toString().trim(); }
