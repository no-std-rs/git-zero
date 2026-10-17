# Local repository hosting measurements

Measured locally on 2026-10-17 with Rust runners and real Git clients. This report
covers the native hosting prototype and a tuned Git smart HTTP reference. No
Cloudflare resources were provisioned or contacted. The [design](hosting-design.md)
separates the working local implementation from the proposed Worker service.
Receive-only medians now edge the reference in both fixtures; initial imports
and full downloads remain close. Incremental reads and cached clones show larger
benefits. Append and some download tails remain higher.

Final raw JSON (gzip): [4 MiB](benchmarks/repository-host-optimized-2026-10-17-4mib.json.gz),
[16 MiB](benchmarks/repository-host-optimized-2026-10-17-16mib.json.gz).
Both use host SHA-256 `fc37b700e7554be9876f29666468bba31cc21e57c36b0404d6b758b2cfb5188d`.
Machine: Linux x86-64, Ryzen 9 7900, 24 logical CPUs, about 125 GiB RAM;
Git 2.53.0, rustc 1.97.1. Full kernel/configuration details are in the raw reports.

## Reproduce and comparison policy

From the workspace root, build once, finish correctness checks, then measure
without overlapping builds or other benchmark runs. The `hosting` profile uses
speed optimization; the portable core/Wasm release profile keeps size optimization.
It retains SHA-256 integrity while reducing an isolated 8.54 MB hash from
[19.8 ms](benchmarks/repository-host-hash-profile-2026-10-17-size.csv) to [4.0 ms](benchmarks/repository-host-hash-profile-2026-10-17-speed.csv).

```sh
cargo build --profile hosting -p git-zero-host --bins --locked
target/hosting/hosting-bench --samples 5 --fixture-mib 4 \
  --pushes 32 --random-fetches 16 --reference-maintenance bitmap \
  --output /tmp/hosting-perf-optimized-4mib.json
target/hosting/hosting-bench --samples 5 --fixture-mib 16 \
  --pushes 32 --random-fetches 16 --reference-maintenance bitmap \
  --output /tmp/hosting-perf-optimized-16mib.json
```

Both backends use the same Rust loopback HTTP runner. The reference invokes
`git http-backend`; the prototype directly invokes stateless Git RPC for ordinary
requests and retains the reference decoding path for encoded requests. Native
Git still validates incoming objects strictly. The prototype's derived Git files
use `core.fsync=none`: acknowledged durability comes from syncing authoritative
immutable blobs, the manifest and directory entries before publishing success.
The reference uses `core.fsync=committed` with the `fsync` method. No power-loss
simulation was performed.

The reference enables bitmaps, reverse indexes and MIDX use, with pack window 10,
depth 50 and automatic compression threads. Default `bitmap` mode performs a
separately timed `git repack -adb --write-bitmap-index` after each measured push.
The **receive-only** comparison excludes that optional work; prototype push times
include all inline compaction, publication and cleanup. `--reference-maintenance
retained` instead retains accepted packs without repacking after measured pushes.
Both modes fully repack the reference with a bitmap after untimed probe population,
so distinct fetches face a prepared reference. Enabling MIDX alone does not
create its bitmap; these reference fixtures use a single-pack bitmap.

Each corpus has 64 KiB files mixing deterministic text and pseudo-random blocks:
64 files / 4 MiB or 256 files / 16 MiB. Thirty-two successive commit tips exercise
an initial push, an incremental push and 30 small appends. Sixteen additional
branch tips fork from varied ancestors; their setup pushes are outside receiving
measurements. Every backend starts from fresh storage, has one warmup, then five
paired trials in alternating AB/BA order. OS caches are warm; they are not dropped.
There is one client using protocol v0.

A cold clone has a cold application response cache. A warm clone repeats its
request into a new client directory, including indexing and checkout time.
Distinct fetches use fresh bare clients and distinct wants; 12 have a seeded
ancestor and four have no haves. Seeding is untimed. Each fetch must install a
nonempty pack; its refs, tree, `fsck --strict` and object reads are checked outside
timing. These cases exercise actual selection beyond identical-response caching.

## Final results

Wall time **median / p95**, milliseconds. Prototype pushes include inline work;
Git receiving excludes separately timed repacks. Initial/clone/incremental rows
have five observations per backend; appends have 150, distinct fetches 80,
including 20 without haves and 60 with haves.

| Operation | 4 MiB prototype | 4 MiB Git | 16 MiB prototype | 16 MiB Git |
| --- | ---: | ---: | ---: | ---: |
| Initial push | 105.92 / 110.16 | 107.61 / 108.80 | 143.69 / 144.74 | 145.80 / 150.67 |
| Incremental push | 30.48 / 30.99 | 32.72 / 33.44 | 54.59 / 55.58 | 55.15 / 56.87 |
| Append push | 22.44 / 31.29 | 24.13 / 24.88 | 24.24 / 29.42 | 25.56 / 26.75 |
| Cold clone | 70.11 / 72.45 | 73.24 / 77.30 | 141.96 / 144.95 | 142.48 / 144.23 |
| Cached clone | 63.69 / 64.01 | 73.87 / 75.41 | 116.95 / 184.98 | 142.13 / 143.51 |
| Incremental fetch | 21.82 / 22.09 | 26.32 / 26.69 | 36.28 / 36.94 | 39.94 / 41.85 |
| Distinct fetch, all | 24.35 / 41.66 | 29.35 / 45.81 | 25.39 / 114.11 | 30.84 / 113.33 |
| Distinct fetch, no haves | 40.82 / 42.54 | 45.15 / 46.51 | 112.06 / 115.63 | 110.68 / 115.26 |
| Distinct fetch, with haves | 20.78 / 38.10 | 25.21 / 43.15 | 20.85 / 40.88 | 26.02 / 44.50 |

Receive medians improve by 1–7%; distinct seeded-fetch medians by 18–20%.
The 16 MiB full-fetch median is 1.2% slower, and its overall distinct-fetch p95
is slightly higher. Append p95 is higher in both corpora: compaction requests
have higher typical tails. The worst append stalls, 164 / 116 ms, occurred
without compaction and spent 138 / 92 ms in storage. The 16 MiB cached-clone
p95 is also worse; median wins do not establish uniform tail-latency gains.

Sum each trial's 32 push times before taking the median, milliseconds:

| Corpus | Prototype, all inline work | Git receive only | Git + optional per-push bitmap repacks |
| --- | ---: | ---: | ---: |
| 4 MiB | 837.48 | 865.61 | 1,251.21 |
| 16 MiB | 942.74 | 969.79 | 1,939.99 |

The last column measures the stated preparation policy, not work Git needs to
acknowledge pushes. It must not replace the receive-only comparison.

## Storage and read costs

First measured trial, after 32 pushes and before probe setup; bytes unless noted:

| Quantity | 4 MiB | 16 MiB |
| --- | ---: | ---: |
| Prototype active compressed packs + indexes | 2,699,302 | 10,191,729 |
| Prototype authoritative files | 2,701,387 | 10,194,272 |
| Prototype global unique-inode files | 2,701,752 | 10,194,637 |
| Prototype allocated unique-inode blocks | 2,740,224 | 10,244,096 |
| Git unique-inode files | 2,183,025 | 8,600,279 |
| Inline compactions / total milliseconds | 9 / 12.44 | 8 / 7.23 |

Authoritative storage is 24% / 19% larger than fully repacked Git. Self-contained
segment bases still duplicate compressed data. The 32 pushes issue 66 blob PUTs
in either corpus, supplying 8.47 / 13.61 MB; cleanup deletes 56 / 50 retired chunks.
Across the distinct read phase, every trial has zero POST response-cache hits,
16 misses, zero persistent byte/file growth and zero blob PUTs. Cache maxima are
8,629,143 / 26,443,243 bytes and 17 / 16 entries, within 32 MiB / 256-entry limits.
The 16 MiB client-installed packs total 1.7% more bytes across 80 distinct fetches;
these sizes include thin-pack completion and are not HTTP wire-byte measurements.

For the cloud-first design, this storage premium is an acceptable candidate
tradeoff for bounded inline work and fewer history rewrites. It is not, by itself,
a reason to add full repacks or cross-segment delta dependencies. Compare retained
GB-months, storage calls, bytes read/rewritten, CPU and request latency for the
expected workload before changing the policy. Local timings and blob counters
do not yet establish a lower Cloudflare bill.

## Why the implementation changed

- Rust creates the disposable bare layout, reads/writes ref snapshots and emits
  protocol-v0 receive advertisements from the authoritative manifest. Direct
  stateless RPC removes repeated native initialization, configuration, ref-list,
  receive-advertisement and HTTP-backend orchestration from the common path.
  Incoming packs of at least 256 KiB overlap chunk hashing with native validation;
  computed digests are reused only after exact comparison with codec output.
  Thin-pack completion and compaction fall back to hashing their resulting bytes.
- Inline compaction validates pack/index SHA-1 checksums, entry CRCs and delta
  dependencies, then copies compressed streams without inflating or recompressing.
  It deduplicates IDs, preserves source physical order, converts offset deltas
  to ID-based deltas, and emits a Git v2 index. Strict validation of untrusted
  incoming objects remains in the receive codec. One job selects four small
  packs with at most 8 MiB of compressed input; it does not repack all history.
- Serving uses automatic thread selection and `pack.window=0` without haves,
  retaining compressed objects/deltas while skipping new delta search. Framed
  requests with haves use window 1, preserving cheap incremental delta selection.
  [Git's implementation](https://github.com/git/git/blob/v2.53.0/builtin/pack-objects.c#L3341-L3362)
  performs object-detail/reuse processing before the zero-window early return.
- The local adapter uses 16 MiB storage chunks; the shared engine's default stays
  8 MiB. Single-chunk native files share immutable blob inodes, including after
  recovery, avoiding copies when a compressed base slightly exceeds 8 MiB.
  Larger packs still need a materialized native file. Durable publication records
  retired chunks before cleanup; successful serialized writes remove replaced
  packs/chunks, and recovery also reclaims unpublished chunks. This reclaims
  superseded storage, not unreachable objects inside a current pack.
- Cached responses share immutable buffers. Fixed-size request hashes, bodies,
  headers and entry overhead count toward 32 MiB; there are at most 256 entries,
  each with a body no larger than 16 MiB. Empty responses are not cached and
  writes invalidate the cache. These are local application-cache bounds.

The [isolated pack profile](benchmarks/repository-host-pack-profile-2026-10-17-16mib.csv)
compares the same 16 MiB probe selection, three repetitions after warmup:

| Threads | Window | Median wall ms | Median user CPU ms | Pack bytes |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 10 | 159.15 | 150 | 8,557,568 |
| Auto | 10 | 43.07 | 130 | 8,557,568 |
| Auto | 0 | 17.85 | 10 | 8,929,115 |

Skipping search adds 4.34% to this pack. These CPU values describe the isolated
`pack-objects` process, unlike the main benchmark's client-only CPU counters;
their 10 ms granularity limits precision. This profile diagnoses server work;
end-to-end fetch results must also include negotiation, transport and the client.
The [incremental profile](benchmarks/repository-host-pack-profile-2026-10-17-incremental.csv)
explains window 1 for haves: median 2.50 ms / 3,366 bytes versus window zero's
20.05 ms / 1,074,316 bytes. The [additional full-fetch profile](benchmarks/repository-host-pack-profile-2026-10-17-full-adaptive.csv)
records the complementary full-fetch tradeoff.

## Measurement limits

Final trial snapshots record load averages of 0.71–1.41 and CPU-pressure avg10=0%.
No agent builds or other benchmark runs intentionally overlapped measurements;
snapshots cannot exclude every in-request scheduling spike on a shared machine.
Medians and nearest-rank p95 have no confidence interval; five-sample p95 is the
largest sample. Append/fetch summaries pool requests across trials, rather than
independent machine-level repetitions. Raw observations/configuration are saved.

Main benchmark GNU `time` counters cover the Git client, excluding the independent
host/server processes. HTTP bodies include framing; installed pack sizes include
thin-pack completion. Blob counters count calls/supplied bytes, not filesystem
writeback or billed R2 operations.

Global inode counts deduplicate hardlinks across services and native view;
do not add component inode totals. Allocated blocks exclude directory metadata
and do not fully represent filesystem compression/shared extents. These small
histories do not establish Linux-scale amplification or maintenance cost.

The local host remains serialized and buffers whole packs/responses. Packs plus
indexes are capped at 128 MiB; refs at 16,384 entries and 4 MiB of names/IDs.
The allocator-free server still lacks receive-pack. Cloudflare wrappers have
mock contract tests; a deployable portable codec, real async bindings, streaming,
paged indexes and concurrent-reader leases remain work. The [Linux-size model](hosting-design.md#linux-sized-cost-model-assumptions-not-a-measurement)
is an estimate, not a Linux benchmark or complete cloud deployment quote.

Historical [4 MiB](benchmarks/repository-host-2026-10-17-4mib.json) and
[16 MiB](benchmarks/repository-host-2026-10-17-16mib.json) reports use five-tip
workloads and retain retired storage; their totals are not paired before/after data.
