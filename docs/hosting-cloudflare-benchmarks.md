# Local Cloudflare services benchmark

This exercises real local R2 and SQLite Durable Object bindings in workerd: ordinary Git clients push and
fetch through a Worker gateway, while the existing Rust host and native Git codec run in a separate loopback
process. It is a pushable local service with measured storage operations. A portable read/write Worker codec
and production cloud measurements remain separate work.

No paid Cloudflare resources are used. Emulator timings describe this local hybrid topology; they do not
predict Cloudflare network latency, isolate CPU, cold starts or a complete hosting bill. The [native hosting
measurements](hosting-performance.md) cover the filesystem-only server comparison.

The warm view removed all R2 GETs from the 16 varied-fetch workload. Its fetch medians were 2–4% slower than
the paired Git reference; pushes remained slower. Cold views repeatedly downloaded the complete repository.
Large retained tails show that this emulator is useful for operation accounting, not a cloud latency
prediction or proof of faster hosting.

## Reproduce

Build and run correctness checks before starting measurements. Start Wrangler with local bindings using the
[Cloudflare service instructions](../examples/repository-host/cloudflare/README.md). The benchmark executable
and fixture/integrity runner are Rust; the JavaScript file is the Worker runtime adapter.

```sh
cargo build --profile hosting -p git-zero-host --bins --locked --offline
target/hosting/hosting-bench \
  --cloudflare-endpoint http://127.0.0.1:8798 \
  --cloudflare-token git-zero-local-services --view-cache cold \
  --samples 5 --fixture-mib 4 --pushes 32 --random-fetches 16 \
  --monthly-runs 1000 --output /tmp/hosting-cloudflare-4mib-cold.json
```

Repeat with `--view-cache warm`, then repeat both policies with `--fixture-mib 16`. Run the four profiles
sequentially without builds or correctness suites in parallel. Each profile uses fresh repository namespaces,
one warmup per backend and five paired trials in alternating order against the same tuned Git smart HTTP
reference. Git bitmap repacks are timed separately; receive comparisons exclude that maintenance.
Configuration and binary hashes are recorded in each report.

Each measured run pushes 32 successive main tips. Read probes then publish 16 distinct branches outside push
timings and fetch them into separately seeded bare clients with varied ancestor haves. Every probe must
install a nonempty pack. Ref/tree checks, strict fsck and archive reads run outside request timings. Probe
population and server verification fetches still generate real operations and remain in the whole-trace cost
scenario.

## Cache and accounting policy

`cold` rebuilds the disposable native object view from actual R2 GETs for uncached reads. The runner requires
positive observer-excluded R2 read bytes. `warm` retains that native filesystem view, which can serve Git
object reads without R2 calls. This view is separate from the response cache's 32 MiB and 256-entry limits.
Repository pack/index bytes are capped at 128 MiB.

Counters record real binding attempts, transferred bytes, failures, Durable Object calls and SQLite rows.
Worker gateway and service-proxy requests are separate: one Git request can cause several service Worker
invocations in this hybrid topology. Pricing uses those actual invocations. The future single-Worker codec
would have a different request/CPU profile.

Metrics and object-statistics probes are labeled `stats`. Raw snapshots retain their operations; workload
deltas and cost scenarios subtract that observer phase. Worker-runtime epoch changes and incomplete operation
receipts fail the benchmark. Actual per-request DO receipts keep counters complete across ordinary DO idle
eviction without extra accounting SQL writes; the DO instance epoch is separate. Read probes also require
unchanged authoritative inventory and zero R2 PUT/DELETE attempts. Local inode bytes describe a disposable
codec cache; authoritative R2 inventory is reported as cloud objects and bytes, separately from Durable Object
state storage.

## Cost interpretation

Per-operation figures use actual observer-excluded counters. Gross linear request/SQL-row cost per 1,000
operations excludes storage, account minimums, allowances, billing rounding and unmeasured compute.

The whole-trace scenario repeats observed traffic 1,000 times and holds the observed R2 peak continuously for
30 days. It does not multiply retained bytes by 1,000 or predict future repository growth. Account allowances
are shared across workloads. Reports include alternative unused-allowance/new-paid-account and
exhausted-allowance/existing-paid-account scenarios, with rounding shown separately from gross linear usage.

R2 Standard storage and operation classes follow [R2 pricing](https://developers.cloudflare.com/r2/pricing/).
Requests and measured SQLite rows follow [Workers
pricing](https://developers.cloudflare.com/workers/platform/pricing/) and [Durable Objects
pricing](https://developers.cloudflare.com/durable-objects/platform/pricing/). Rates are verified for
2026-10-17. Worker CPU, Durable Object active duration, monthly SQLite storage and the separate native codec
host's compute remain unpriced. Full-bill totals are explicitly unknown; known subtotals are not a complete
monthly quote.

## Results: 2026-10-17

Five paired samples per profile, one excluded warmup, 32 pushes and 16 varied fetches per run. Linux, Ryzen 9
7900, 24 logical CPUs, Git 2.53.0; Wrangler 4.149.0, Miniflare 5.20261006.1-alpha, workerd 1.20261006.1. All
four reports completed with validated content, unchanged authoritative read-phase storage, no service errors,
and complete service accounting.

Native binary SHA-256: `4017f6321d491e72e4dd29e857c0d855607738752b8505a8fe005f636eb62d69`. Worker source
SHA-256: `e8c088cff457f86cef5a25bbf18e7a17f002e3a0cbac586671778875ca6b6f22`. The source stayed frozen and all
trials used one Worker-runtime accounting epoch.

Raw reports: [4 MiB cold](benchmarks/repository-host-cloudflare-2026-10-17-4mib-cold.json.gz), [4 MiB
warm](benchmarks/repository-host-cloudflare-2026-10-17-4mib-warm.json.gz), [16 MiB
cold](benchmarks/repository-host-cloudflare-2026-10-17-16mib-cold.json.gz), [16 MiB
warm](benchmarks/repository-host-cloudflare-2026-10-17-16mib-warm.json.gz).

Times below are median (p95), milliseconds. Append pools 150 requests and varied fetch pools 80; initial and
incremental pushes each have five samples. Git bitmap maintenance is outside its receive times; hybrid
maintenance is inline.

| Fixture / view | Backend | Initial push | Incremental push | Append push | Varied fetch |
| --- | --- | ---: | ---: | ---: | ---: |
| 4 MiB cold | Hybrid | 140.49 (518.70) | 56.98 (821.66) | 58.09 (111.26) | 52.63 (90.74) |
| 4 MiB cold | Git | 107.58 (109.36) | 32.48 (32.87) | 24.11 (24.87) | 29.47 (45.80) |
| 4 MiB warm | Hybrid | 139.45 (6737.71) | 148.08 (828.67) | 35.22 (82.36) | 30.23 (47.43) |
| 4 MiB warm | Git | 107.02 (109.14) | 33.18 (33.73) | 24.26 (24.96) | 29.69 (45.32) |
| 16 MiB cold | Hybrid | 539.52 (27673.63) | 548.88 (4207.09) | 97.70 (330.01) | 97.61 (256.71) |
| 16 MiB cold | Git | 150.45 (176.90) | 56.03 (72.74) | 25.94 (32.05) | 30.54 (118.67) |
| 16 MiB warm | Hybrid | 249.54 (371.17) | 76.37 (85.93) | 36.79 (72.52) | 31.85 (120.64) |
| 16 MiB warm | Git | 149.68 (152.60) | 56.52 (57.37) | 25.86 (26.82) | 30.75 (113.60) |

The 16 MiB cold trials had surrounding load averages of 2.70–22.43 and CPU pressure avg10 of 0–15.41%; the
other profiles recorded avg10 CPU pressure of zero. These snapshots do not explain every in-request stall. The
worst cold initial push took 27,673.63 ms; recorded receive prepare/codec/storage/publication accounted for
only 221.91 ms. The remainder is uninstrumented client, gateway, dispatch and service work. Its DO instance
changed while aggregate counters remained complete. The 4 MiB warm clone tail was 7,313.93 ms with zero codec
work because the response was cached. Separately, its worst append took 7,151.75 ms, including 7,097.99 ms in
receive storage work. All tails are retained; neither CPU contention nor R2 alone explains every delay.

### Objects and read amplification

Both policies had identical inventories immediately after 32 pushes, before probe publication. Sizes include
compressed packs and indexes.

| Fixture | Unique Git objects | Encoded / duplicate / delta representations | Packs | R2 keys | R2 bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 4 MiB | 198 | 218 / 20 / 69 | 5 | 10 | 2,699,302 |
| 16 MiB | 414 | 468 / 54 / 93 | 8 | 17 | 10,191,729 |

After adding 16 probe branches, the 4 MiB repository had 262 unique objects (118 blobs, 48 commits, 96 trees),
299 representations including 37 duplicates and 101 deltas, and 6 R2 keys totaling 3,241,859 bytes. The 16 MiB
repository had 478 unique objects (334 blobs, 48 commits, 96 trees) and 125 deltas. Cold stored 551
representations/73 duplicates in 13 keys totaling 10,750,836 bytes; warm stored 547/69 in 13 keys totaling
10,727,215 bytes. Uncompressed logical object bytes were 7,850,214 and 22,326,822 respectively. All objects
were reachable. Peak R2 storage, including temporary overlap, was 4,739,013 / 11,191,107 bytes. Current DO
SQLite size was 20,480 bytes; monthly retention cost remains unknown.

| Fixture / view | Actual GETs for 16 fetches | R2 bytes read | Worker / DO requests |
| --- | ---: | ---: | ---: |
| 4 MiB cold | 102 | 55,111,603 | 166 / 134 |
| 4 MiB warm | 0 | 0 | 64 / 32 |
| 16 MiB cold | 234 | 193,515,048 | 298 / 266 |
| 16 MiB warm | 0 | 0 | 64 / 32 |

Cold 4 MiB performs 17 complete rehydrates: 16 POST cache misses and the first advertisement GET miss. Cold 16
MiB performs 18: inserting the fourth full response evicts the oldest advertisement under the 32 MiB FIFO
budget; probe 13 then misses it again. Each probe has exactly one advertisement GET and one POST. R2 read
bytes were 6.38× / 5.53× HTTP response bytes respectively. This full-view policy is unsuitable for large
repositories; selective pack/range reads and bounded materialized caches remain necessary. Warm avoids those
R2 reads by retaining the declared native filesystem cache.

All 16 distinct POST requests missed the response cache in each trial. Maximum resident cache use was
8,629,148 / 26,443,274 bytes and 17 / 16 entries, below 32 MiB and 256 entries. Fetches issued no PUTs/deletes
and did not grow R2 inventory. Stats expose Git type/delta/duplicate inventories and bounded R2 key
access/hot-key counts; these are not per-decoded-Git-object popularity.

### Actual operation counts and costs

Cells show mean; median when different, excluding observers. A/B are R2 classes; W/DO are Worker and Durable
Object requests. SQL R/W shows mean rows read/written. Known USD is gross linear request/SQL-row cost per
1,000 operations, excluding storage, rounding, allowances and unknown compute.

| Fixture / view | Operation | A / B: mean; median | W / DO: mean; median | SQL R / W mean | Known USD / 1,000 |
| --- | --- | ---: | ---: | ---: | ---: |
| 4 MiB cold | Initial push | 2 / 0 | 7 / 5 | 6 / 5 | $0.01685600 |
| 4 MiB cold | Incremental push | 2 / 2 | 9 / 7 | 6 / 5 | $0.01847600 |
| 4 MiB cold | Append push | 2.0667 / 7.6; 2 / 8 | 16.5333 / 14.5333; 15 / 13 | 7.9333 / 5.1333 | $0.02431727 |
| 4 MiB cold | Varied fetch | 0 / 6.375; 0 / 6 | 10.375 / 8.375; 10 / 8 | 2 / 0 | $0.00666575 |
| 4 MiB warm | Initial push | 2 / 0 | 7 / 5 | 6 / 5 | $0.01685600 |
| 4 MiB warm | Incremental push | 2 / 0 | 7 / 5 | 6 / 5 | $0.01685600 |
| 4 MiB warm | Append push | 2.0667 / 0; 2 / 0 | 8.9333 / 6.9333; 7 / 5 | 7.9333 / 5.1333 | $0.01816127 |
| 4 MiB warm | Varied fetch | 0 / 0 | 4 / 2 | 2 / 0 | $0.00150200 |
| 16 MiB cold | Initial push | 3 / 0 | 8 / 6 | 7.8 / 7 | $0.02380780 |
| 16 MiB cold | Incremental push | 2 / 3 | 10 / 8 | 6 / 5 | $0.01928600 |
| 16 MiB cold | Append push | 2.0667 / 10.2; 2 / 11 | 18.9333 / 16.9333; 18 / 16 | 7.6 / 5 | $0.02619960 |
| 16 MiB cold | Varied fetch | 0 / 14.625; 0 / 13 | 18.625 / 16.625; 17 / 15 | 2 / 0 | $0.01334825 |
| 16 MiB warm | Initial push | 3 / 0 | 8 / 6 | 7 / 7 | $0.02380700 |
| 16 MiB warm | Incremental push | 2 / 0 | 7 / 5 | 6 / 5 | $0.01685600 |
| 16 MiB warm | Append push | 2.0667 / 0; 2 / 0 | 8.7333 / 6.7333; 7 / 5 | 7.6 / 5 | $0.01793760 |
| 16 MiB warm | Varied fetch | 0 / 0 | 4 / 2 | 2 / 0 | $0.00150200 |

The following scenario replays the whole trace 1,000 times, including fixture population and verification,
while retaining observed peak R2 bytes once for 30 days. It assumes an existing paid account with exhausted
allowances. Linear and whole-unit-rounded figures are alternatives, not amounts to add.

| Fixture / view | A / B per trace | W / DO per trace | Known gross linear USD | Rounded known subtotal | Rounded R2-only share |
| --- | ---: | ---: | ---: | ---: | ---: |
| 4 MiB cold | 71 / 374 | 755 / 649 | $0.961378085 | $6.2525 | $4.875 |
| 4 MiB warm | 71 / 0 | 381 / 275 | $0.658438085 | $5.7803 | $4.515 |
| 16 MiB cold | 70 / 610 | 982 / 876 | $1.138071867–$1.138073867 | $6.3206 | $4.875 |
| 16 MiB warm | 70 / 0 | 372 / 266 | $0.643971867 | $5.7776 | $4.515 |

With unused account allowances, known usage charges are zero for these scenarios, plus a $5 new paid-account
baseline. The full bill is unknown because Worker CPU, DO active duration, monthly SQLite storage and the
separate codec host are unpriced. Existing invoice-unit rounding slack can reduce incremental charges. See the
pricing sources and account assumptions above.

### Try the local repository

The local demo uses the same emulator bindings. From a repository with commits:

```sh
git push http://127.0.0.1:8798/demo/repo.git HEAD:refs/heads/main
git clone http://127.0.0.1:8798/demo/repo.git demo-clone
curl --fail http://127.0.0.1:8910/_object-stats
```

For a fresh session, start the services and native host as described in the [Cloudflare
README](../examples/repository-host/cloudflare/README.md). The benchmark uses disposable namespaces. On
failure it retains local fixtures, an operation journal and an explicitly incomplete diagnostic report;
incomplete counters are never converted to a valid cost estimate.
