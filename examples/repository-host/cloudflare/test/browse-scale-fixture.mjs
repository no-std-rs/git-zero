import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
export function git(dir, args, input) {
  const result = spawnSync("git", args, { cwd: dir, input, maxBuffer: 256 * 1024 * 1024, timeout: 120000, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" } });
  assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`); return result.stdout;
}
export async function generated(root, count, format, realistic) {
  const dir = join(root, `${count}-${format}-${realistic}`); await mkdir(dir);
  git(dir, ["init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`]);
  const commands = [];
  for (let i = 1; i <= count; i++) {
    if (realistic && i === 1) for (let n = 0; n < 24; n++) {
      const module = `# framework module ${n}\n`;
      commands.push(`blob\nmark :${1000000 + n}\ndata ${Buffer.byteLength(module)}\n${module}`);
    }
    const body = `${realistic ? "# analysis framework\n".repeat(30) : ""}analysis revision ${i}\n`;
    const extra = realistic && i === 1 ? Array.from({ length: 24 }, (_, n) => `M 100644 :${1000000 + n} src/framework/${n < 12 ? "models" : "data"}/module-${n}.py\n`).join("") : "";
    commands.push(`blob\nmark :${i * 2}\ndata ${Buffer.byteLength(body)}\n${body}commit refs/heads/main\nmark :${i * 2 + 1}\nauthor Author <a@example.test> ${i} +0000\ncommitter Committer <c@example.test> ${i} +0000\ndata ${String(i).length + 8}\ncommit ${i}\nM 100644 :${i * 2} ${realistic ? "src/framework/models/analysis.py" : "src/analysis.py"}\n${extra}\n`);
  }
  // A distinct branch is published and then rewritten, retaining its old bytes.
  commands.push(`reset refs/heads/other\nfrom :${Math.floor(count / 2) * 2 + 1}\n\nreset refs/tags/release\nfrom :${count * 2 + 1}\n\nblob\nmark :${count * 2 + 2}\ndata 7\nsecret\ncommit refs/heads/rewritten\nmark :${count * 2 + 3}\ncommitter Old <o@example.test> 1 +0000\ndata 9\nold fork\nfrom :3\nM 100644 :${count * 2 + 2} secret\n\ndone\n`);
  git(dir, ["fast-import", "--quiet", "--done"], commands.join(""));
  const id = (name) => git(dir, ["rev-parse", name]).toString().trim();
  return { dir, tip: id("refs/heads/main"), old: id("refs/heads/rewritten"), secret: id("refs/heads/rewritten:secret"), refs: git(dir, ["for-each-ref", "--format=%(refname) %(objectname)"]).toString().trim().split("\n").map((line) => { const [name, oid] = line.split(" "); return { name, oid }; }) };
}
