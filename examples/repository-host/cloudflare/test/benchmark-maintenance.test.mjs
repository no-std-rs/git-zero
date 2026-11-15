import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { Git, fixture, reference, MAINTENANCE } from "../scripts/benchmark-git.mjs";
const repositoryRoot = fileURLToPath(new URL("../../../../", import.meta.url));
for (const format of ["sha1", "sha256"]) test(`reference benchmark owns pack maintenance without concurrent automatic writers ${format}`, { timeout: 60000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-reference-maintenance-")), source = join(root, "source.git"), directory = join(root, "pair");
  const git = new Git(process.env.GIT_ZERO_BENCH_TEST_GIT ?? "git"), trace = join(root, "trace.json");
  git.env.GIT_TRACE2_EVENT = trace;
  let server;
  try {
    const tips = await fixture(source, git, { objectFormat: format, generations: 4, blobBytes: 4096 }); await mkdir(directory);
    server = await reference(directory, repositoryRoot, git, format, { trace2: trace }); const repo = join(directory, "repo.git");
    for (const [index, tip] of tips.entries()) {
      await git.run(source, ["push", "--atomic", server.origin, tip + ":refs/heads/main", tip + ":refs/heads/history-" + index]);
      for (const command of MAINTENANCE) await git.run(repo, command);
      await git.run(repo, ["multi-pack-index", "verify"]);
      await git.run(repo, ["commit-graph", "verify"]);
    }
    await server.stop(); server = null;
    const events = (await readFile(trace, "utf8")).trim().split("\n").map((line) => JSON.parse(line));
    const competing = events.filter((row) => row.event === "child_start" && row.argv?.includes("--auto") && (row.argv.includes("maintenance") || row.argv.includes("gc")));
    assert.deepEqual(competing.map((row) => row.argv), [], "automatic writers must not race the benchmark's measured MIDX/graph phase");
    const packs = (await readdir(join(repo, "objects/pack"))).filter((name) => name.endsWith(".pack"));
    assert.equal(packs.length, 4, "only incoming packs and explicit index/graph maintenance belong to this workload");
    await git.run(repo, ["fsck", "--strict"]);
    const ids = async (where) => (await git.text(where, ["rev-list", "--objects", "--no-object-names", "refs/heads/main"])).split("\n").sort();
    assert.deepEqual(await ids(repo), await ids(source));
  } finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
