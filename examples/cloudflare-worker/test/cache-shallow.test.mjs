import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { createPackCache } from "../src/pack-cache.mjs";
import { createRepositoryWorker } from "../src/repository.mjs";
import { backend } from "./cache-fixture.mjs";
import { execute, module, withFixture } from "./revision-fixture.mjs";
import { withServer } from "./http.mjs";

const oracle = fileURLToPath(new URL("../../../tests/worker_shallow.py", import.meta.url));
for (const format of ["sha1", "sha256"]) {
  for (const kind of ["kv", "r2"]) {
    test(`Worker ${kind} cached ${format} shallow/filter/revision matrix matches Git HTTP backend`, { timeout: 180000 }, async () => {
      await withFixture(format, async ({ root, repository }) => {
        const packCache = createPackCache({ backend: kind, binding: backend(kind), namespace: "shallow-oracle" });
        const worker = createRepositoryWorker(module, (request) => {
          const url = new URL(request.url);
          return { repository, target: url.pathname.slice("/repo.git/".length) + url.search, packCache,
            revisionLimits: { maxExclusions: 8, graphReadLimit: 1048576 } };
        });
        await withServer((request) => worker.fetch(request), async (origin) => {
          const result = await execute("python3", [oracle, "--root", root, "--zero-url", `${origin}/repo.git`], { timeout: 150000 });
          assert.match(result.stdout, /absolute\/relative\/filter\/time\/exclusion oracle passed/);
        });
        const statistics = packCache.statistics();
        assert.ok(statistics.misses > 0);
        assert.ok(statistics.hits > 0);
        assert.equal(statistics.writes, statistics.misses);
        assert.equal(statistics.failures, 0);
        assert.equal(statistics.inflight, 0);
      });
    });
  }
}
