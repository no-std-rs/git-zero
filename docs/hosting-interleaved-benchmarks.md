# Interleaved hosting benchmark v1

`hosting-bench --flow interleaved-v1` selects a separate schema-v4 workload.
The default batch runner and its schema-v3 cost objective retain their existing
behavior. Never compare costs across the two flows.

The four interleaved profiles are **16/64 MiB × cold/warm codec view**. Each
paired trial starts fresh and performs exactly **32 pushes and 56 fetches**:

- Every generation atomically updates `main` and publishes `history-{generation}`,
  then a persistent bare client fetches the current `main`.
- After generations 4, 8, …, 32, two fresh clients fetch distinct, previously
  published historical refs. One has no objects; the other receives a strictly
  older ancestor locally, outside the timer.
- A third fresh client, seeded at the preceding main generation outside the
  timer, fetches current `main` again. This requires a nonempty transfer and
  allows an exact-response cache hit.

A fixed xorshift seed selects historical wants and older haves. The fixture
records all generation commit/tree IDs, the entire operation order, and its
SHA-256. Each timed fetch uses `fetch --keep` and `fetch.unpackLimit=1`, must
install a nonempty pack, and passes wanted-ref/tree verification, strict fsck
and archive inflation outside its timer. Local ancestor seeds use the same pack
retention settings. Reports record `config.fetch_unpack_limit=1`, and the scorer
requires it. On Git 2.53, a small HTTP download with `--keep` writes loose objects
at limit 0 and retains a pack at limit 1; the same file-transport fetch retains a
pack at both limits. A three-commit real-Git HTTP regression covers initial and
incremental downloads by fresh and ancestor-seeded clients. Validated temporary
history/recheck clients are removed immediately. There are no post-batch setup
pushes. The initial attempt with a zero limit failed during its first warmup,
before any valid pair or score; its diagnostic report remains incomplete.

The initial payload is 50% compressible. Generations 5, 9, 13, 17, 21, 25 and 29
replace an 8 MiB file using independent deterministic contents. Its Git
`-delta` attribute prevents delta encoding against previous versions. Both
servers must observe initial wire bytes of at least 40% of logical fixture size,
and at least 3 MiB for each later large push. Other generations make small
mutations. Existing body, snapshot and response-cache caps remain in force.

Stock Git uses the fixed `midx-bitmap-graph-v1` reference policy. Its receiver
adds `pack.allowPackReuse=multi` to the existing configuration, including
`receive.fsckObjects=true`, `core.fsync=committed` and `core.fsyncMethod=fsync`.
The effective receiver configuration is queried and recorded for every trial.
After **every** push, before the next fetch, it runs these two commands in order:

```sh
git multi-pack-index write --bitmap
git commit-graph write --reachable --split
```

Accepted packs remain in place. The MIDX command does not use `--incremental`,
which is incompatible with `--bitmap`. Each existing maintenance row retains
both command timings and resources. Its wall time sums both commands; measured
CPU times sum and maximum RSS takes the larger peak. An unavailable command
resource measurement leaves the aggregate null. Reports preserve wire-push
latency, maintenance latency and their sum, `ready_wall_ms`; the push guard uses
readiness. Prototype inline maintenance is already inside its wire-push time.
Latency groups are `large_push`, `small_push`, `latest_fetch`, `history_no_haves`,
`history_with_haves`, and `latest_recheck`.

The interleaved flow defaults to this exact policy and rejects other
`--reference-maintenance` values. Its objective identity records the policy.
The batch flow still defaults to `bitmap` with its existing full-repack behavior.
An interleaved-only `--reference-binary PATH` chooses the Git HTTP wrapper;
it defaults to the adjacent `repository-host`. Freeze one reference binary and
pass it to both baseline and candidate runs. Its SHA-256 stays in comparison
identity, while its filesystem path is provenance. The runner checks that the
binary remains unchanged, and the scorer rejects mixed reference builds.

Every push checks response-cache invalidation and actual manifest-generation
advance. Every read compares actual R2 inventory and complete counter epochs,
and rejects Class A operations, deletes, written bytes, or SQLite row writes.
Every cold latest fetch must perform actual R2 GETs and read positive bytes.
Per-push metrics include actual compaction deltas. Every native trace must
observe at least one compaction, with its per-push count sum matching final
native metrics; zero-compaction traces cannot score. Lightweight existing metrics
supply inventory and SQLite generation; the full reachable-object inventory is
checked once at completion. Raw observer counters remain in the trace and are
excluded by the pricing model. The response cache stays at 32 MiB / 256 entries
with a 16 MiB maximum response.

A full run defaults to five measured pairs (`--stage full`). A screen requires
exactly two (`--stage screen`) and can only produce an inconclusive comparison.
A screen cannot establish improvement or count toward a plateau. The scorer
rejects missing operations, changed schedules, mixed flows, mixed stages,
changed fixture/cache identities, incomplete accounting and unverified reads.

For example, after building the Rust binaries and starting the local Worker:

```sh
hosting-bench --flow interleaved-v1 --stage full --fixture-mib 16 \
  --cloudflare-endpoint http://127.0.0.1:8808 --view-cache cold \
  --reference-binary /path/to/frozen/repository-host \
  --output interleaved-16mib-cold.json
```

Repeat for the other three profiles. Pass all four reports to `hosting-cost`;
its schema-v2 objective prices each profile's exact 32:56 mix repeated 1,000
times, plus its observed R2 peak held once for 30 days. Full comparisons retain
the existing ≥1% known-cost improvement, separated trial envelopes, read-byte
and storage guards, and paired Git-normalized median/p95 limits. Unknown cloud
compute remains unknown. Local timings are latency guards, not billed compute.

## Iteration budget

The user selected two-pair screens for each optimization, followed by five-pair validation of the final
candidate. Keep a matching two-pair scalar baseline for screening. Use complete screen counters and
resource/latency guards to choose the next provisional candidate; do not treat the CLI's screen decision
as formal acceptance. Each change still has a clean designer, clean implementation, committed and pushed
source, and independent measurement.

Stop screening when two independent viable designs fail to lower known cost by 1%, or a separately
substantiated bound rules out a meaningful remaining saving. Invalid, resource-regressing and noisy or
incomparable attempts do not establish that condition. Then measure the original scalar baseline and
final candidate with five pairs and the identical runner/reference/runtime policy. Report the full
comparison's outcome and all earlier screens; no screen counts as a formally accepted result or formal
plateau by itself. A failed final latency guard remains inconclusive rather than becoming a speed claim.

## Isolated GitHub runner experiment

The [isolated workflow](../.github/workflows/hosting-isolated.yml) runs only on explicit pushes to
`perf/hosting-isolated-ci-20261018`, or dispatches selecting that branch. Each of the four profiles gets
a standard `ubuntu-24.04` VM. On that same VM, it freshly measures scalar baseline
`556b9ecd2aa79197f86b3db2d546eee81696e899` followed by the pushed candidate, with a fresh local Wrangler
process and persistent-state directory for each provider. These are a new environment's measurements;
local-workstation reports must not serve as their baseline.

All dependencies and both native variants are built before timing, with two build jobs and no artifact
or dependency caches. Both providers use the candidate's identical Rust runner, the frozen scalar
reference wrapper, Git 2.53.0 and the same pinned Worker source/configuration. Standardized absolute build
paths and one Rust stable toolchain preserve binary identity across profile jobs; the existing Rust
scorer rejects any mixed native/reference hashes. Health and runtime checks finish before timing, and
no background benchmark probes run. The complete storage, compaction, cache, fsck and nonempty-pack
requirements remain enforced by the existing runner and scorer.

Branch pushes default to `screen`, with exactly two measured pairs per provider/profile. For the final
five-pair run, change the workflow's single `BENCH_STAGE` fallback from `'screen'` to `'full'` in a reviewed
commit and push that commit to the same branch. This works even when GitHub cannot dispatch a workflow
that has never existed on the default branch. If dispatch is available, its explicit `stage` input
overrides the fallback. Stage and sample count are the only workload changes for final validation.
Future candidates can include the workflow on the same branch and repeat the complete fresh comparison.

The offline Rust scoring job compares all four profiles and retains every actual resource and latency
guard in `comparison.json`; a screen remains formally inconclusive. A green workflow only means that
measurement and scoring completed. Inspect the comparison decision and reasons before interpreting it.
Unknown Worker CPU, DO duration, monthly SQLite storage and native compute remain unknown.

Per-profile evidence contains compressed raw JSON, incomplete progress journals when present, source
and executable hashes, runtime/hardware provenance and local service logs. A separate small archive
holds scores and evaluator provenance. Artifacts have one-day retention and a hard 4 MiB compressed
cap per archive (at most five archives, 20 MiB total); no binaries, fixtures, emulator state or caches are uploaded.
Download evidence before expiration. No Cloudflare account, deployment, secrets or paid runner is used.
