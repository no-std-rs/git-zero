import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

export function gitEnvironment(options = {}) {
  return { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_"))),
    GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0",
    GIT_AUTHOR_NAME: "git-zero", GIT_COMMITTER_NAME: "git-zero", GIT_AUTHOR_EMAIL: "git-zero@example.invalid", GIT_COMMITTER_EMAIL: "git-zero@example.invalid",
    GIT_AUTHOR_DATE: "@0 +0000", GIT_COMMITTER_DATE: "@0 +0000",
    ...(options.dumb ? { GIT_SMART_HTTP: "0" } : {}) };
}

export function git(directory, args, input, options = {}) {
  const result = spawnSync(process.env.GIT_ZERO_TEST_GIT ?? "git", ["--git-dir", directory, ...args], { env: gitEnvironment(options), input, timeout: 30000 });
  assert.equal(result.status, 0, result.stderr?.toString());
  return result.stdout;
}

export function oracle(directory, files) {
  git(directory, ["init", "--bare", "--initial-branch=main", "--object-format=sha1"]);
  const groups = new Map();
  for (const [path, body] of files) {
    const [group, name] = path.split("/");
    const id = git(directory, ["hash-object", "-w", "--stdin"], body).toString().trim();
    if (!groups.has(group)) groups.set(group, []);
    groups.get(group).push(`100644 blob ${id}\t${name}\0`);
  }
  const roots = [];
  for (const [group, entries] of groups) roots.push(`040000 tree ${git(directory, ["mktree", "-z"], entries.join("")).toString().trim()}\t${group}\0`);
  const root = git(directory, ["mktree", "-z"], roots.join("")).toString().trim();
  const commit = git(directory, ["commit-tree", root], "Tree metadata snapshot\n").toString().trim();
  git(directory, ["update-ref", "refs/heads/main", commit]);
  git(directory, ["fsck", "--strict"]);
  return commit;
}
