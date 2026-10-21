import { testRuntimes } from "./runtimes.mjs";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { promisify } from "node:util";
import { headers, TOKEN } from "./wrangler.mjs";

const execute = promisify(execFile);
async function git(cwd, ...args) {
  const result = await execute("git", ["-c", "core.fsync=none", "-c", "maintenance.autoDetach=false", "-c", "gc.autoDetach=false", "-c", "user.name=Tree client", "-c", "user.email=tree@example.test", "-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, ...args], {
    cwd, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" }, timeout: 30000, maxBuffer: 4 * 1024 * 1024,
  });
  return result.stdout.trim();
}

testRuntimes("{runtime} supports ordinary tree-filter clones and lazy checkout", { timeout: 120000 }, async (serve) => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-tree-clients-")); let server;
  try {
    server = await serve(join(root, "state"));
    for (const format of [0, 1]) {
      const source = join(root, `source-${format}`), namespace = `tree-clients-${format}`, remote = `${server.origin}/${namespace}.git`;
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }); assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      await git(root, "init", "-q", "-b", "main", `--object-format=${format ? "sha256" : "sha1"}`, source);
      const paths = ["root.txt", "a/deep/file", "z-short/file"];
      for (const name of paths) { await mkdir(dirname(join(source, name)), { recursive: true }); await writeFile(join(source, name), name === "root.txt" ? "root data\n" : "shared leaf\n"); }
      await git(source, "add", "."); await git(source, "commit", "-q", "-m", "tree-filter fixture");
      await git(source, "push", "--atomic", remote, "HEAD:refs/heads/main");
      for (const version of [0, 1, 2]) for (const depth of [0, 1, 2]) {
        const filter = `tree:${depth}`, clone = join(root, `clone-${format}-${version}-${depth}`);
        await git(root, "-c", `protocol.version=${version}`, "clone", "-q", "--no-checkout", "--single-branch", "--no-tags", `--filter=${filter}`, remote, clone);
        assert.equal(await git(clone, "config", "remote.origin.partialclonefilter"), filter);
        await git(clone, "-c", `protocol.version=${version}`, "checkout", "-q", "main");
        for (const name of paths) assert.deepEqual(await readFile(join(clone, name)), await readFile(join(source, name)), `${format}/${version}/${depth}/${name}`);
        await git(clone, "fsck", "--full", "--strict");
      }
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
