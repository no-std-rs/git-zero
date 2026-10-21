import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { entry, object, pack, project } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

async function git(args, input) {
  const child = spawn("git", ["-c", "core.fsync=none", "-c", "user.name=Tag test", "-c", "user.email=tag@example.test", "-c", `http.extraHeader=Authorization: Bearer ${TOKEN}`, ...args], {
    timeout: 30000, killSignal: "SIGKILL", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" },
  });
  let stdout = "", stderr = "";
  child.stdout.on("data", (bytes) => { stdout += bytes; }); child.stderr.on("data", (bytes) => { stderr += bytes; });
  child.stdin.on("error", () => {}); child.stdin.end(input);
  const status = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
  assert.equal(status, 0, stderr); return stdout.trim();
}

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`${runtime} pushes and fetches valid tag chains beyond 128 levels`, { timeout: 120000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-deep-tags-")), persist = join(root, "state"); let server;
  try {
    server = await serve(persist);
    for (const format of [0, 1]) {
      const fixture = project(format), objects = [...fixture.objects], source = join(root, `source-${format}`), clone = join(root, `clone-${format}`), namespace = `deep-${format}`;
      let target = fixture.root;
      for (let depth = 0; depth < 257; depth++) {
        target = object(format, 4, `object ${target.id.toString("hex")}\ntype ${depth ? "tag" : "commit"}\ntag level-${depth}\ntagger A <a@b> 0 +0000\n\nvalid nested tag\n`);
        objects.push(target);
      }
      await git(["init", "--bare", `--object-format=${format ? "sha256" : "sha1"}`, source]);
      const sourceArgs = ["--git-dir", source];
      await git([...sourceArgs, "index-pack", "--stdin", "--strict"], pack(format, objects.map((item) => entry(item.kind, item.body))));
      await git([...sourceArgs, "update-ref", "refs/heads/main", fixture.root.id.toString("hex")]);
      await git([...sourceArgs, "update-ref", "refs/tags/deep", target.id.toString("hex")]);
      await git([...sourceArgs, "fsck", "--full", "--strict"]);
      const initialized = await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) });
      assert.equal(initialized.status, 200); await initialized.arrayBuffer();
      const remote = () => `${server.origin}/${namespace}.git`;
      await git([...sourceArgs, "push", "--atomic", remote(), "refs/heads/main", "refs/tags/deep"]);
      for (const version of [0, 1, 2]) assert.ok((await git([...sourceArgs, "-c", `protocol.version=${version}`, "ls-remote", remote(), "refs/tags/deep*"])).includes(`${fixture.root.id.toString("hex")}\trefs/tags/deep^{}`));
      await server.stop(); server = await serve(persist);
      await git(["-c", "protocol.version=2", "clone", remote(), clone]);
      await git(["-C", clone, "fsck", "--full", "--strict"]);
      assert.equal(await git(["-C", clone, "rev-parse", "deep^{}"]), fixture.root.id.toString("hex"));
      await git([...sourceArgs, "tag", "-a", "outer", "deep", "-m", "published nested target"]);
      await git([...sourceArgs, "push", remote(), "refs/tags/outer"]);
      await git(["-C", clone, "fetch", "--tags", remote()]);
      await git(["-C", clone, "fsck", "--full", "--strict"]);
      assert.equal(await git(["-C", clone, "rev-parse", "outer^{}"]), fixture.root.id.toString("hex"));
    }
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
