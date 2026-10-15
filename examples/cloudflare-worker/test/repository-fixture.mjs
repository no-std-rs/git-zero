import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { after } from "node:test";

export const execute = promisify(execFile);
export const gitBinary = process.env.GIT_ZERO_TEST_GIT ?? "git";
export const directory = await mkdtemp(join(tmpdir(), "git-zero-worker-objects-"));
after(() => rm(directory, { recursive: true, force: true }));
export const fixtureRoot = join(directory, "fixtures");
await execute("python3", [fileURLToPath(new URL("./fixture.py", import.meta.url)), "--root", fixtureRoot, "--git", gitBinary], { timeout: 60000 });
const report = JSON.parse(await readFile(join(fixtureRoot, "report.json"), "utf8"));

export function object(format, kind, body) {
  const bytes = Buffer.from(body);
  const id = createHash(format).update(`${kind} ${bytes.length}\0`).update(bytes).digest("hex");
  return { id, kind, body: bytes };
}

export const fixtures = await Promise.all(report.map(async (row) => {
  const exported = join(fixtureRoot, row.format, "export");
  const lines = (await readFile(join(exported, "objects.txt"), "utf8")).trim().split("\n");
  const objects = await Promise.all(lines.map(async (line) => {
    const [id, kind] = line.split("\t");
    const body = await readFile(join(exported, "raw", id));
    assert.equal(object(row.format, kind, body).id, id, "Git export canonical ID disagrees with independent Node hash");
    return { id, kind, body };
  }));
  const refs = (await readFile(join(exported, "refs.txt"), "utf8")).trim().split("\n").map((line) => {
    const [name, id, symbolic, peeled] = line.split("\t");
    return { name, id, symbolicTarget: symbolic === "-" ? null : symbolic, peeled: peeled === "-" ? null : peeled };
  });
  assert.equal(objects.length, 44);
  assert.equal(refs.length, 7);
  assert.equal(objects.reduce((sum, value) => sum + value.body.length, 0), row.rawBytes);
  assert.equal(Math.max(...objects.map((value) => value.body.length)), 98304);
  return { ...row, objects, refs, head: { type: "symbolic", target: row.headTarget, id: row.head }, source: join(fixtureRoot, row.format, "source.git") };
}));

export function gitEnvironment(format, dumb = false) {
  return {
    ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_"))),
    GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0", GIT_DEFAULT_HASH: format,
    ...(dumb ? { GIT_SMART_HTTP: "0" } : {}),
  };
}

export function synthetic(format) {
  const blobs = Array.from({ length: 61 }, (_, index) => object(format, "blob", `value-${index}\n`));
  function tree(last) {
    return object(format, "tree", Buffer.concat([...blobs, last].map((value, index) => Buffer.concat([
      Buffer.from(`100644 f${String(index).padStart(3, "0")}\0`), Buffer.from(value.id, "hex"),
    ]))));
  }
  const commit = (root) => object(format, "commit", `tree ${root.id}\nauthor Fixture <fixture@example.invalid> 0 +0000\ncommitter Fixture <fixture@example.invalid> 0 +0000\n\nFull arena\n`);
  const placeholder = tree(object(format, "blob", ""));
  const length = 131072 - blobs.reduce((sum, value) => sum + value.body.length, 0) - placeholder.body.length - commit(placeholder).body.length;
  const large = object(format, "blob", Buffer.alloc(length, 0xa7));
  const root = tree(large);
  const tip = commit(root);
  const repository = {
    format, objects: [tip, root, ...blobs, large],
    refs: Array.from({ length: 8 }, (_, index) => ({ name: `refs/heads/b${index}`, id: tip.id })),
    head: { type: "symbolic", target: "refs/heads/b7", id: tip.id },
  };
  assert.equal(repository.objects.length, 64);
  assert.equal(repository.objects.reduce((sum, value) => sum + value.body.length, 0), 131072);
  return repository;
}
