import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { createRepositoryWorker } from "../src/repository.mjs";
import { withServer } from "./http.mjs";
import { directory, fixtures, execute, gitBinary, gitEnvironment } from "./repository-fixture.mjs";
import { exercisePromised, withGitServer } from "./promised-oracle.mjs";

const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));
test("real Git partial clones lazily retrieve visible payloads in both formats and v0/v1/v2", { timeout: 120000 }, async () => {
  for (const fixture of fixtures) {
    const repository = { ...fixture, refs: fixture.refs.filter(ref => ref.name !== "refs/tags/inner") };
    await execute(gitBinary, ["-C", fixture.source, "update-ref", "-d", "refs/tags/inner"], { env: gitEnvironment(fixture.format) });
    let reference;
    await withGitServer(repository, async url => { reference = await exercisePromised(repository, url, join(directory, `promised-git-${fixture.format}`)); });
    const worker = createRepositoryWorker(module, async request => ({ repository,
      target: new URL(request.url).pathname.replace(/^\/repo.git\//, "") + new URL(request.url).search,
      wantPolicy: "all-visible-reachable", maxWantLines: 64 }));
    await withServer(request => worker.fetch(request), async url => {
      const actual = await exercisePromised(repository, url, join(directory, `promised-worker-${fixture.format}`), true);
      assert.deepEqual(actual, reference); console.log(JSON.stringify({ promised: actual }));
    });
  }
});
