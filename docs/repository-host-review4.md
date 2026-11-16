# Embeddable repository host: review verification

These local measurements cover the fourth review of PR #66. The HTTP and
embedding contract is [repository-host-v1.md](repository-host-v1.md).
Git 2.53.0 and Node 24.10.0 generated the fixtures. Both local Worker runtimes
use the pinned no-alloc Wasm codec; no production deployment was performed.

## Findings and regressions

| Finding | Result and regression |
| --- | --- |
| 1. Tenant fairness and route sizing | Namespace rotation, no incumbent heavy replenishment ahead of another namespace, protected metadata arena/working bytes, route-sized reservations, no preemption. `admission.test.mjs` holds two clones while 300 browse reads complete, tests larger waiters and blob streams, and admits control phases behind two fetches/thirty cached streams. |
| 2. Token compatibility | Token mode bypasses byte admission by default. `concurrent-git.test.mjs` runs two real clones of different repositories on independent slow links and asserts both streams overlap. |
| 3. Pack input lifetime | Total time applies to controls/JSON after admission; pack input uses throughput credit. `admission.test.mjs` exceeds a scaled former total and separately waits 100 ms for a gate and 80 ms for memory under a 40 ms control deadline. `review-availability.test.mjs` still rejects trickles promptly. |
| 4. Ref/version scaling | Covering oid/peeled interval indexes, live intervals first, indexed retired-version pruning, safe old snapshots, bounded yielding. `ref-scaling.test.mjs` publishes 5,000 distinct annotated tags, clones with real Git, checks all refs/fsck and explicit include-tag, and models 10,000 closed intervals. |
| 5. Shared build failures | Builder owns its job before preparation; initiator cancellation cannot stop followers. Pre-header failures fall back, and generation uses a rate-based bound. `shared-pack.test.mjs` tests cancellation before preparation, independent fallback, cleanup failures, and 256 chunks beyond initial grace. |
| 6. Measured memory | Default 64 MiB pool, reusable charged arenas and up-front legacy chunk allowance. `workerd-memory.test.mjs` samples the actual isolate under two live fetches, 30 live cached readers and 100 additional cached-fetch admissions. `codec.test.mjs` checks reset across formats, frozen controls, partial graph state and budgets. |
| 7. Cold full-clone preparation | Compiled SQL templates, indexed pending frontier and synchronous scratch transactions of at most 128 nodes. `clone-scale.test.mjs` uses 25k commits/150k default-packed objects, imports the wire pack with Git and compares the complete reachable object set after a rewrite. `sql-rewrite.test.mjs` measures template lookup overhead. |
| 8. Capacity/status | Cached readers have a separate ceiling and release processing slots. Scratch contention returns 503 with Retry-After before headers. `shared-pack.test.mjs`, `concurrent-git.test.mjs` and both-format `fetch-quota.test.mjs` cover these paths. |
| 9. Bounds/proofs/contract | Binding fields overwrite colliding proof fields; realistic SQL bounds and depth-adjusted growth checks are enforced. `ref-scaling.test.mjs` and `browse-scale.test.mjs` cover this. The contract explains control/data deadlines, index rebuilding, erasure, admission and cache behavior. Paired timings below retain the original test deadlines. |

The security behavior accepted in earlier reviews remains covered: grants at
arrival, direct-stub re-verification, scoped EU identity, namespace retirement,
neighboring bucket prefixes, unreachable/rejected objects, typed graph links,
proof epochs/floors and atomic publication records.

## Read API scale

The generated default-packed fixture has 25,000 commits, 150,028 stored objects,
24 framework modules, directory depth three and delta depth up to 50. A rewritten
branch leaves old objects stored. Real Git supplies refs, tree, blob and log
oracles. Statement counts include admission/lifecycle checks, not billing rows.

| Ref-rooted operation | 1k commits | 25k commits | Row writes |
| --- | ---: | ---: | ---: |
| Commit | 14 | 14 | 0 |
| History, 20 entries | 70 | 70 | 0 |
| Directory | 154 | 70 | 0 |
| Blob by nested path | 202 | 76 | 0 |

The undeltified SHA-1 and SHA-256 fixtures have identical 1k/25k counts:
14/70/18/18, with zero writes. The bound is **24 + 5 × delta-work**, where
delta-work sums (1 + verified delta depth) over requested/walked object ranges.
The realistic growth assertion allows only the corresponding depth difference
plus 32 fixed statements. It does not multiply every row by the entire
repository's maximum delta depth. Work grows with returned rows, path/tag depth
and delta reconstruction, independently of total repository history.

The raw absence/reachability index completes in 63 requests for the wide/deep
25k fixture (18 for the undeltified fixture); alarms advance the same bounded
work. Browser links immediately after publication use signed proofs and need
zero building retries. After a destructive publication, non-tip raw Git wants
may be refused until alarms rebuild membership, about a minute at this size.
Erasure requires complete repository deletion and permanent namespace retirement;
rewrites alone retain packs and old IDs in history/log state.

## Memory samples

The gateway payload ledger peaked at **60.94 MiB**, with three retained arenas
reserving 18 MiB and actually using **17.0625 MiB** of Wasm linear memory. The
pool cap is 64 MiB; the fourth arena remains available for metadata. No active
job is cancelled merely because another namespace waits.

The inspector reports these separate categories (MiB). They are samples, not
process RSS or a measured worst-case peak; compiled code/runtime allocations
and the embedding product's own Rust/Wasm must also fit in the 128 MB isolate.

| Phase | JS used heap | Array-buffer backing storage | Embedder heap | Wasm linear bytes |
| --- | ---: | ---: | ---: | ---: |
| Two live generation streams | 21.20 | 6.37 | 8.49 | 17.06 |
| Thirty cached readers added | 5.17 | 3.55 | 0.36 | 17.06 |
| After 100 further cache-hit admissions | 8.38 | 22.34 | 1.44 | 17.06 |
| After explicit GC, streams still live | 2.86 | 4.34 | 0.29 | 17.06 |

Each route is also sampled before/after a read. The simple fixture's observed
JS-used-plus-backing increases were approximately 0.17 MiB for commit, 0.15 MiB
for history, 0.19 MiB for tree and 2.38 MiB for a 1 MiB random blob. These are
small-fixture allocation samples, not their worst-case reservation. Default
reservations are 14/19/17/14 MiB respectively (12 MiB for a blob by oid), derived
from the configured metadata/JSON/cache/chunk bounds. Git controls reserve
11 MiB, fetch generation 14 MiB, and push intake adds four times the validated
control-prefix size. A cached reader reserves 192 KiB and no codec. Prefix
buffers and owned caches are reclaimed explicitly; idle arenas stay charged.

Repeated forced-GC calls on an idle workerd inspector sometimes did not return
in the measurement harness. The sampler therefore records route samples
without forcing collection between them, then performs one post-burst GC while
jobs remain live. Inspector commands have a five-second failure bound. The
host's memory policy does not invoke GC or depend on this diagnostic protocol.

To size an embedded deployment, subtract the product's own measured steady
memory, allocator burst and VM/HTTP/SQL allowance from 128 MB before choosing
the host pool. Lower caches, controls, pages, pins and arena count together;
startup rejects profiles that cannot fit a compatible host job and fair share.
A smaller profile has less concurrency. Use a separate Worker when the combined
application cannot fit its required concurrency. The 64 MiB default leaves
64 MiB outside host payload reservations; it does not reserve that remainder
exclusively for the product's linear memory.

## Clone and ref costs

On local workerd/S3, the 25k fixture prepared a full current-reachable clone in
**26.043 s**: selection 22.410 s, preparation 3.633 s; headers arrived at 26.058 s.
The wire pack was 11,774,780 bytes with 150,025 reachable objects. Git's strict
indexer accepted it and its complete object set matched Git after the rewrite.
The private SQL view compiled 21 templates and reused 537,840 lookups. Earlier
unbatched/poorly indexed preparation took 162 s; progress deadlines remain
necessary for larger repositories or slower storage. These are local costs,
not a production size/latency guarantee.

A 50,000-statement Node microbenchmark measured median former regex rewriting
at 1.42 microseconds/statement versus 0.016 microseconds for a cached template,
excluding SQLite/I/O. The distinct 5,000-tip test took 60 ms for indexed lookups
and 58 ms for 5,000 peeled lookups behind 10,000 closed versions; its real Git
clone took 1.70 s. Retired versions prune in indexed batches of at most 512,
never beyond the oldest live snapshot.

One fairness run completed 300 concurrent browse reads behind two held clones;
the light namespace's worst observed request was 17.75 ms. Both clones survived,
and the ledger peaked at 56 MiB. Under default settings, one namespace cannot
consume the final ordinary-metadata allowance or every arena. Multiple saturated
namespaces and oversized/legacy jobs can still get retryable 503 after bounded
waits; there is no CPU/network SLA or contention preemption.

The real-Git workshop test keeps sixteen clones live, checks a consistent old
snapshot while browse/push complete, races same/different refs and deletes under
live readers. Representative runs at roughly 5 Mbit/s/client were:

| Runtime | One clone | Sixteen clones | Browse | Concurrent push | New builds |
| --- | ---: | ---: | ---: | ---: | ---: |
| Wrangler R2/SQLite | 15.507 s | 14.680 s | 40 ms | 73 ms | 1 |
| Direct workerd/S3/SQLite | 13.582 s | 15.835 s | 55 ms | 485 ms | 1 |

Two token-mode repositories cloned concurrently on independent slow links in
19.884 s, with both streams observed live. Token mode has no byte-pool admission
by default. Raising maxConcurrentJobs cannot create memory; cached readers use
a separate maxCachedReaders ceiling (64 default), leaving processing slots for
push/browse during a workshop.

## Interleaved tree-depth measurements

The comparison baseline is main commit
`9efd2f70da9247411feb6bd79d4b291cd618c6b6`, built with its own codec. The script
records both runtimes, all samples and load; no other test workload is launched
by this agent during the pairs. External machine activity is not controlled.

Times below are seconds. Each pair alternates main/head order; both tests keep
their existing 120-second deadlines. All twenty runtime tests passed.

| Pair | Wrangler main | Wrangler head | Direct workerd main | Direct workerd head |
| --- | ---: | ---: | ---: | ---: |
| 1 | 8.493 | 8.646 | 8.262 | 8.556 |
| 2 | 8.606 | 8.599 | 8.487 | 8.429 |
| 3 | 8.501 | 8.706 | 8.387 | 8.535 |
| 4 | 8.598 | 8.661 | 8.495 | 8.593 |
| 5 | 9.798 | 8.842 | 8.862 | 8.483 |
| **Median** | **8.598** | **8.661** | **8.487** | **8.535** |

The medians increased by 0.7% and 0.6% respectively. The earlier single-run
62.6-to-89.7-second change did not recur. The last pair saw machine load rise
from about 1.5 to 8.1 on 24 available CPUs; earlier pairs stayed below 1.6.
This controlled comparison does not establish the cause of the historical
timeouts or socket resets. Raw samples/load readings are generated under
`.scratch/review4-tree-timings/` by `scripts/review-tree-timings.mjs`.

## Reproducing verification

From the repository root, create `.scratch/test-tmp` and set TMPDIR to its
absolute path. Keep large fixtures there and remove repositories/packs after
measurement, retaining small logs. No fixtures or Wasm binaries are committed.

```sh
cargo test --workspace --locked
python3 -m unittest discover -s tests -p 'test_*.py'
RUSTFLAGS=-Dwarnings python3 scripts/check-features.py
python3 scripts/check-public-packages.py
```

Then run npm ci and npm run test:streamed in
examples/repository-host/cloudflare. The final exact-source local run passed
251/251 host tests in 398.754 s. Rust workspace tests, 96 Python tests, all 39
isolated feature builds with warnings denied, and public-package checks passed.
Focused regressions and the paired timing script are listed in tests/README.md.
No existing test deadline, batching assertion, lint or fuzz target was loosened
to obtain these results. Historical timeout tracking remains
both runtime variants of tree-depths.test.mjs; the separate old socket-reset
reports involved Wrangler tree-depth and direct-workerd wanted-closure tests.
The fourth-review runs pass these without changing their original deadlines;
the exact cause of the historical resets was not established.
