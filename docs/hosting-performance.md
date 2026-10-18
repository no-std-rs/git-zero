# Hosting benchmarks and cost model

The Rust runners compare the [repository host](../examples/repository-host/README.md)
with a tuned Git smart HTTP server. Generated reports belong in `/tmp` or CI
artifacts, outside the source tree. The [isolated workflow](../.github/workflows/hosting-isolated.yml)
retains reports as artifacts instead of committing them.

The runner generates deterministic repository fixtures in the system temporary
directory; no sample repository download is needed. Successful runs remove them
unless `--keep` is set; failed runs retain them for diagnosis. Larger upstream
datasets should be downloaded on demand into `/tmp` or `test-repositories/`.

## Reproduce

Build both runners and the host:

```sh
cargo build --profile hosting -p git-zero-host --bins --locked
```

For a native comparison:

```sh
target/hosting/hosting-bench --samples 5 --fixture-mib 4 \
  --pushes 32 --random-fetches 16 \
  --output /tmp/git-zero-native-benchmark.json
```

For Cloudflare operation counts, start the [local emulator](hosting-cloudflare-services.md#run).
Run each of the four interleaved profiles: 16/64 MiB fixtures, each with
`cold` and `warm` native views. For example:

```sh
target/hosting/hosting-bench --cloudflare-endpoint http://127.0.0.1:8798 \
  --cloudflare-token git-zero-local-services --cloudflare-repo-prefix localbench \
  --flow interleaved-v1 --stage screen --view-cache cold --samples 2 \
  --fixture-mib 16 --pushes 32 --random-fetches 56 \
  --output /tmp/hosting-16-cold.json
```

Use two measured pairs for `--stage screen`; final validation uses
`--stage full --samples 5`. A screen cannot establish improvement or a plateau.
When comparing builds, freeze one reference host and pass its path through
`--reference-binary` to every interleaved run. Keep runner, Git version, fixture,
cache policy and runtime consistent. The workflow shows the pinned CI setup.

## Workload and comparison

Each interleaved trial starts fresh with 32 pushes and 56 verified fetches.
Every push updates `main` and a historical ref, followed by a latest fetch.
Every fourth generation adds historical fetches by fresh and ancestor-seeded
clients, then another latest fetch. Deterministic large replacements mix with
small pushes. Verification checks wanted refs/trees, strict Git fsck and archive
inflation outside the timer; historical fetches must transfer actual packs.

The reference maintains MIDX bitmaps and a split commit graph after each push.
Reports separate wire latency from maintenance and compare push readiness,
including maintenance time. Storage, read amplification, cache bounds and
Git-normalized median/p95 latency remain acceptance guards alongside dollars.
A successful benchmark job alone does not establish a performance win.

## Estimate cost

Score all four reports together, then compare against a matching baseline:

```sh
target/hosting/hosting-cost --output /tmp/hosting-score.json \
  /tmp/hosting-16-cold.json /tmp/hosting-16-warm.json \
  /tmp/hosting-64-cold.json /tmp/hosting-64-warm.json
# Add --baseline /tmp/baseline-score.json to produce a comparison.
```

The interleaved objective prices 1,000 repetitions of each profile's 32:56
push/fetch mix, plus its observed R2 peak retained once for 30 days. Counters
include binding attempts, bytes and SQLite rows; startup and diagnostic work
remain visible separately. The dated rate card and validation rules live in
[`cost.rs`](../examples/repository-host/src/cost.rs) and
[`benchmark_cost.rs`](../examples/repository-host/src/benchmark_cost.rs).

Costs are gross linear resource estimates with no allowance or subscription
adjustment. Worker CPU, Durable Object duration, monthly SQLite storage and
native codec compute remain unknown, so the complete bill stays unknown.
Local workerd timings measure emulator overhead rather than production cloud
latency. Full validation and Linux repository-scale support remain pending;
these runners do not establish that the host is faster than tuned Git.
