import assert from "node:assert/strict";
import { readFile, readlink, lstat, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { cache, handler } from "./cache-fixture.mjs";
import { withServer } from "./http.mjs";
import { directory, execute, fixtures, gitBinary, gitEnvironment } from "./repository-fixture.mjs";

// Create each ref/HEAD state independently with Git, then export the exact
// reachable closure. No imported object is injected into a client after cloning.
async function variant(fixture, state) {
  const root = join(directory, `head-fixture-${fixture.format}-${state}`);
  await mkdir(root);
  const source = join(root, "repo.git");
  const git = (...args) => execute(gitBinary, args, { env: gitEnvironment(fixture.format), encoding: "buffer", timeout: 30000 });
  if (state.startsWith("unborn")) {
    const target = state === "unborn-main" ? "main" : "new/default";
    await git("init", "--bare", "-q", `--object-format=${fixture.format}`, `--initial-branch=${target}`, source);
    return { repository: { format: fixture.format, objects: [], refs: [], head: { type: "unborn", target: `refs/heads/${target}` } }, source, root };
  }
  await git("clone", "--mirror", "-q", fixture.source, source);
  if (state.startsWith("detached")) {
    await git("-C", source, "update-ref", "--no-deref", "HEAD", fixture.head.id);
    if (state === "detached-only") {
      for (const ref of fixture.refs) {
        // Delete symbolic aliases themselves rather than their targets.
        await git("-C", source, "update-ref", "--no-deref", "-d", ref.name);
      }
    }
  } else if (state === "symbolic-chain") {
    await git("-C", source, "symbolic-ref", "refs/heads/alias1", fixture.head.target);
    await git("-C", source, "symbolic-ref", "refs/heads/alias2", "refs/heads/alias1");
    await git("-C", source, "symbolic-ref", "HEAD", "refs/heads/alias2");
  }
  const lines = (await git("-C", source, "for-each-ref", "--format=%(refname) %(objectname) %(*objectname)")).stdout.toString().trim().split("\n").filter(Boolean);
  const refs = [];
  for (const line of lines) {
    const [name, id] = line.split(" ");
    const symref = await git("-C", source, "symbolic-ref", "--no-recurse", "-q", name).then((value) => value.stdout.toString().trim(), () => null);
    const kind = (await git("-C", source, "cat-file", "-t", id)).stdout.toString().trim();
    const peeled = kind === "tag" ? (await git("-C", source, "rev-parse", `${name}^{}`)).stdout.toString().trim() : null;
    refs.push({ name, id, symbolicTarget: symref, peeled });
  }
  const target = await git("-C", source, "symbolic-ref", "--no-recurse", "-q", "HEAD").then((value) => value.stdout.toString().trim(), () => null);
  const id = (await git("-C", source, "rev-parse", "HEAD")).stdout.toString().trim();
  const head = target ? { type: "symbolic", target, id } : { type: "detached", id };
  const ids = new Set((await git("-C", source, "rev-list", "--objects", "--no-object-names", "HEAD", "--all")).stdout.toString().trim().split("\n"));
  const objects = fixture.objects.filter((value) => ids.has(value.id));
  assert.equal(objects.length, ids.size);
  // The base fixture has seven refs; chain mode needs two extra slots. Remove
  // independent annotations, retaining the nested tag and all referenced tips.
  if (refs.length > 8) {
    const extra = refs.findIndex((value) => value.name.startsWith("refs/tags/") && !value.peeled);
    assert.ok(extra >= 0);
    await git("-C", source, "update-ref", "-d", refs[extra].name);
    refs.splice(extra, 1);
  }
  return { repository: { format: fixture.format, objects, refs, head }, source, root };
}

for (const fixture of fixtures) {
  for (const state of ["unborn-main", "unborn-nested", "detached-only", "detached-refs", "symbolic-chain"]) {
    test(`Worker cached ${fixture.format} ${state} discovery/clone/fetch agrees with independent Git`, () => checkHeadState(fixture, state));
  }
}

async function checkHeadState(fixture, state) {
  const { repository, source, root } = await variant(fixture, state);
  const { binding, policy } = cache();
  const worker = handler(repository, policy);
  await withServer((request) => worker.fetch(request), async (origin) => {
    const url = `${origin}/repo.git`;
    for (const version of [0, 1, 2]) {
      await compareHeadVersion({ repository, source, root, binding, url, version });
    }
  });
}

async function compareHeadVersion({ repository, source, root, binding, url, version }) {
  const git = (...args) => execute(gitBinary, ["-c", `protocol.version=${version}`, ...args], {
    env: gitEnvironment(repository.format), encoding: "buffer", timeout: 30000,
  });
  const expected = (await git("ls-remote", "--symref", source)).stdout.toString().trim().split("\n").sort();
  const actual = (await git("ls-remote", "--symref", url)).stdout.toString().trim().split("\n").sort();
  assert.deepEqual(actual, expected, `v${version} HEAD/ref advertisement`);
  const head = await fetch(`${url}/HEAD`).then((response) => response.text());
  assert.equal(head, repository.head.type === "unborn" || repository.head.type === "symbolic" ? `ref: ${repository.head.target}\n` : `${repository.head.id}\n`);
  const reference = join(root, `reference-${version}`), clone = join(root, `worker-${version}`);
  await git("clone", "-q", "--no-local", source, reference);
  await git("clone", "-q", url, clone);
  await git("-C", clone, "fsck", "--strict");
  await git("-C", reference, "fsck", "--strict");
  const refs = async (path) => (await git("-C", path, "for-each-ref", "--format=%(refname) %(objectname)")).stdout.toString().trim().split("\n").sort();
  assert.deepEqual(await refs(clone), await refs(reference));
  const symbolic = (path) => git("-C", path, "symbolic-ref", "-q", "HEAD").then((value) => value.stdout.toString().trim(), () => null);
  assert.equal(await symbolic(clone), await symbolic(reference));
  const objects = async (path) => (await git("-C", path, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)")).stdout.toString().trim().split("\n").filter(Boolean).sort();
  assert.deepEqual(await objects(clone), await objects(reference));
  if (repository.head.type === "unborn") {
    assert.deepEqual(await objects(clone), []);
    assert.equal((await git("-C", clone, "rev-parse", "--show-object-format")).stdout.toString().trim(), repository.format);
    assert.equal(binding.operations.length, 0);
  } else {
    assert.equal((await git("-C", clone, "rev-parse", "HEAD")).stdout.toString().trim(), repository.head.id);
    const tree = (path) => git("-C", path, "ls-tree", "-r", "-z", "HEAD").then((value) => value.stdout);
    const expectedTree = await tree(reference);
    assert.equal(Buffer.compare(await tree(clone), expectedTree), 0);
    for (const row of expectedTree.toString().split("\0").filter(Boolean)) {
      const [metadata, path] = row.split("\t");
      const mode = metadata.split(" ")[0];
      const file = join(clone, path), oracle = join(reference, path);
      if (mode === "120000") assert.equal(await readlink(file), await readlink(oracle));
      else {
        assert.equal(Buffer.compare(await readFile(file), await readFile(oracle)), 0);
        assert.equal(Boolean((await lstat(file)).mode & 0o111), Boolean((await lstat(oracle)).mode & 0o111));
      }
    }
    await git("-C", clone, "fetch", "-q", "origin", "HEAD");
    await git("-C", clone, "fsck", "--strict");
    assert.equal((await git("-C", clone, "rev-parse", "FETCH_HEAD")).stdout.toString().trim(), repository.head.id);
  }
}
