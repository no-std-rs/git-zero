// Invoked by the native integration test with its independently exported fixture.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { exercisePromised, git, withGitServer } from "./promised-oracle.mjs";
const [root, format, url] = process.argv.slice(2);
const report = JSON.parse(await readFile(join(root, "fixtures/report.json"), "utf8"));
const row = report.find(row => row.format === format);
const exported = join(root, "fixtures", format, "export");
const objects = await Promise.all((await readFile(join(exported, "objects.txt"), "utf8")).trim().split("\n").map(async line => {
  const [id, kind] = line.split("\t"); return { id, kind, body: await readFile(join(exported, "raw", id)) };
}));
const refs = (await readFile(join(exported, "refs.txt"), "utf8")).trim().split("\n").map(line => {
  const [name, id] = line.split("\t"); return { name, id };
}).filter(ref => ref.name !== "refs/tags/inner");
const repository = { ...row, objects, refs, head: { id: row.head }, source: join(root, "fixtures", format, "source.git") };
await git(format, "-C", repository.source, "update-ref", "-d", "refs/tags/inner");
let reference;
await withGitServer(repository, async control => { reference = await exercisePromised(repository, control, join(root, `git-${format}`)); });
const actual = await exercisePromised(repository, url, join(root, `native-${format}`), true);
assert.deepEqual(actual, reference);
console.log(JSON.stringify({ nativePromised: actual }));
