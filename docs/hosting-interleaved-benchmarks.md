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
candidate. Measure a fresh matching two-pair baseline for each screen: the scalar provider initially,
then the preceding provisional candidate. The declared batch below permits one fresh intermediate
suite to serve both adjacent comparisons within the same workflow attempt. Use complete screen counters and
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
a standard `ubuntu-24.04` VM. A screen freshly measures its declared ordered native providers on
the same VM, once each with two measured pairs and an excluded warmup per backend. The completed
R2–R5 and R5–R8 batches each used four providers. Every provider gets a fresh local
Wrangler process and persistent-state directory. No reports from an earlier
workflow attempt or local workstation may substitute for any provider's measurements.

All dependencies and native variants are built before timing, with two compiler jobs and no artifact
or dependency caches. Every provider uses the frozen checkout's identical final Rust runner and
backward-compatible Worker, the frozen scalar
reference wrapper, Git 2.53.0 and the same pinned Worker source/configuration. Standardized absolute build
paths and one Rust stable toolchain preserve binary identity across profile jobs; the existing Rust
scorer rejects any mixed native/reference hashes. Health and runtime checks finish before timing, and
no background benchmark probes run. The complete storage, compaction, cache, fsck and nonempty-pack
requirements remain enforced by the existing runner and scorer.

Before pushing each batch, validate the common final Worker's backward compatibility and pin its
validated hash alongside every actual pushed source ref. For the earlier R2–R5 batch, generation
witnesses were gated by registration capability so R2/R3 incurred no extra DO checks, and 16 MiB chunk
support still accepted old 8 MiB entries within the unchanged aggregate response limit. The completed R5–R8 batch
kept R5 scalar retirement unless negotiated. The next R8–R11 batch keeps older
clients on their negotiated upload path and fresh v1 catalog; only R11 requests
the peak-catalog schema, with migration charged to its first eligible mutation.

`REFERENCE_COMMIT` stays fixed at scalar `556b9ecd2aa79197f86b3db2d546eee81696e899` for every round.
Its source remains at `scalar-baseline` and its build at `baseline-target` under the experiment root,
preserving the reference executable's original build paths and hash. Screen native hosts build from
their pushed `SCREEN_RN_COMMIT` refs at deterministic `providers/rN` source paths and corresponding
`provider-targets/rN` build paths. The earlier R2–R5 batch used `providers/r2` through `providers/r5`;
the R5–R8 batch retained `providers/r5` and added `providers/r6`, `providers/r7` and `providers/r8`. Duplicate refs or
native source trees block the screen.
Provenance records every label, commit, source tree, manifest path/hash and post-run clean status.

The workflow accepts an ordered `SCREEN_PROVIDERS` list of two to four labels, with a pinned
`SCREEN_RN_COMMIT` for each label. It rejects repeated labels, commits or native source trees
before timing. Later batches use fresh suites and declare their candidate sequence before running;
no screen samples carry over between workflow attempts. Full mode always measures two fresh
providers, the original scalar baseline and the final candidate, with five pairs each.

The earlier R2–R5 batch was prospectively declared with independently prepared candidates. Preparing later source
changes before this batch completes does not mean their design was informed by these measurements.
The same fresh intermediate suite in that workflow attempt serves as a candidate in one comparison
and a baseline in the next: R2→R3, R3→R4 and R4→R5. Those are correlated uses of the same observed
two-pair samples, not independent replications or extra samples. The scorer also emits R4-versus-R2,
R5-versus-R2 and R5-versus-R3 so a later cumulative candidate can be assessed against the actual last
provisionally selected baseline if an earlier step is skipped. This reuse is limited to this declared
batch; subsequent adaptive design must use the resulting evidence and its own declared screen.

Progression is conditional. The coordinator must inspect all resource and latency guards, including
tails, before selecting a provisional state; a green job does not select all changes. Guard failures
require review, and every two-pair comparison remains formally inconclusive without establishing a
formal plateau. Before interpreting comparisons, validate workflow run/attempt, provider refs and
hashes, common runner/Worker/reference identity and the per-profile shared VM/boot identity. The
workflow checks those provenance boundaries in addition to the Rust scorer's four-profile identity
checks. Hardware may differ between profiles; compared providers within each profile must share a VM.

Final validation selects `full`, fixes `BASELINE_COMMIT` to original scalar R1 and compares it with
current HEAD using five fresh measured pairs on every profile. A full run rejects another baseline.
No screen samples can be reused for final validation, and the Git reference stays fixed throughout.

Branch pushes default to `screen`, with exactly two measured pairs per provider/profile. For the final
five-pair run, change the workflow's single `BENCH_STAGE` fallback from `'screen'` to `'full'` in a reviewed
commit and push that commit to the same branch. This works even when GitHub cannot dispatch a workflow
that has never existed on the default branch. If dispatch is available, its explicit `stage` input
overrides the fallback. Fixture, maintenance, cache budgets and guard policies stay unchanged.

The offline Rust scoring job requires all four profiles for every provider and retains all actual
resource and latency guards in provider scores and all earlier-to-later `rN-vs-rM.json` comparisons
(six for each four-provider batch). Full mode writes
`comparison.json`; a screen remains formally inconclusive. A green workflow only means that
measurement and scoring completed. Inspect the comparison decision and reasons before interpreting it.
Unknown Worker CPU, DO duration, monthly SQLite storage and native compute remain unknown.

Per-profile evidence contains compressed raw JSON, incomplete progress journals when present, source
and executable hashes, runtime/hardware provenance and local service logs. A separate small archive
holds scores and evaluator provenance. Artifacts have one-day retention and a hard 4 MiB compressed
cap per archive (at most five archives, 20 MiB total); no binaries, fixtures, emulator state or caches are uploaded.
Download evidence before expiration. No Cloudflare account, deployment, secrets or paid runner is used.

Initial workflow run [39081449226](https://github.com/no-std-rs/git-zero/actions/runs/39081449226),
at `e2a54d69ebcd79a157c77c1979d265fbfd44ee81`, failed workflow validation because job-level `env` does
not expose the `runner` context. No jobs started and no measurements were collected. Experiment paths
now use the available `github.workspace` context at job scope, retaining stable paths outside checkout.

## Completed R5–R8 screen: retirement, verification and discovery

After reviewing the complete R2–R5 screen below, this batch measured the
provisional R5 anchor (`086229b`), R6 fused retirement (`e4cdb32`), R7 single
verification (`50e2fd8`) and R8 certified native discovery (`0f9f62e`). It used
fresh service state and two pairs per provider on each of the same four profile
VMs, with one warmup per backend. Intermediate suites serve both adjacent
comparisons; these comparisons are correlated. All six earlier-to-later
comparisons remain visible. No earlier run's reports are substituted. The common
Worker stays pinned to
`b5ed0af945a3497d31dfbfb449f52e387ddb1f2821a6cdb750656458cc7528d6`;
R5 registrations do not negotiate the new retirement capability.

R6 preserves actual R2 deletions, catalog writes and durable retirement debt while
combining cleanup with the CAS callback. Its three-key real-workerd contract uses
one Worker/DO callback rather than four, with identical R2/SQL work. R7 passes
owned verified chunks through the service wrappers, avoiding a second SHA-256
pass while preserving corruption and exact ordered-identity checks before any
native file materialization. It changes no storage operation or priced counter.
The independent combined correctness record covers 80 package tests, strict
Clippy, and nine real-workerd contracts, including push/fetch, compaction,
HMAC/cache invalidation, typed 16 MiB reads and restart rehydration.

R8 learns the exact native protocol-v0 branch advertisement after an existing
full reconstruction and one bounded native commit-type check. Eligible later
discovery uses authoritative manifest metadata, with no durable metadata or
object cache. Strict publications can advance the certificate; recovery,
external generations, changed policy and unsupported requests fall back.
Actual workerd checks cover byte/header equivalence, zero eligible discovery R2
reads, fetch contents, strict publication, external-generation fallback and the
workflow's exact Git environment. Independent source review found no blocker
after the safe zero-value environment fix. The existing 32 MiB allocation budget
includes the bounded template, reported separately from the response cache.

[Run 39087721879](https://github.com/no-std-rs/git-zero/actions/runs/39087721879)
completed every profile and scoring. Its fresh fixed scores were R5
$2.33324593618, R6 $2.29544193618, R7 $2.29545493618 and R8 $2.05460443618.
R8 is **11.942226% below R5** and 10.492495% below R7. All 216 resource guards
and 24 profile-cost guards pass. R7 versus R6 has no known-dollar improvement;
its tiny SQL-read variance does not price CPU work.

All six comparisons remain inconclusive. R8 versus R5 fails four latency groups:
16 MiB warm historical reads without haves and large pushes, and 64 MiB cold
large pushes and latest rechecks. Its 116.9-second cold historical-fetch tail is
retained. Every profile stays at 24,576 SQLite bytes, with unchanged storage and
wire/pack sizes. R8 avoids exactly 31 full discovery reconstructions per cold
trial, reducing GETs 685→494 at 16 MiB and 907→654 at 64 MiB.

[Published evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r5-r8-screen/README.md)
retains the five original archives, all guards/ratios/tails, counter scopes and
independent exact offline reproduction. After reviewing every guard, R8 is the
provisional known-cost anchor. This establishes neither a speed win nor formal
acceptance or stopping. Final validation still requires five fresh original-R1
and selected-final pairs on all four profiles.

## Completed R2–R5 screen

[Run 39084173778](https://github.com/no-std-rs/git-zero/actions/runs/39084173778)
completed all four profiles and scoring. Independent verification reproduced all
four scores and six comparisons from unchanged raw reports. The known fixed
objective fell from R2 $2.78119593618 through R3 $2.70602893618 and R4
$2.49482893618 to R5 $2.33324193618, a 16.106524% reduction within this batch.
All comparisons pass cost, storage, transfer and read/write resource guards.

Every comparison remains formally inconclusive. Besides the mandatory screen
reason, R5 versus R4 fails four latency groups: 16 MiB cold large pushes, 16 MiB
warm historical fetches with haves, 64 MiB cold large pushes and 64 MiB warm
historical fetches without haves. The last group's 138.5-second p95 is retained.
R5 is the next provisional known-cost anchor, with no acceptance, speed win or
plateau claim. Final confirmation remains five fresh original-R1/final pairs.

[Published evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r2-r5-screen/README.md)
includes compressed profiles/scores, every guard and actual host/Git ratios,
source/VM/binary checks, and explicitly separate fixed and whole-trace counters.
Worker CPU, DO duration, monthly SQLite storage and native compute remain unknown.

[Retained warm-fetch tail](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r5-fetch-tail/README.md)
records the 138.5-second sample alongside its 143.6 ms native codec time and zero
R2 reads. Response completion and client waiting were not traced, so the cause
remains unresolved.

## Prepared next candidates

The [R9 upload/publication record](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r9-upload-candidate/README.md)
retains clean and independent combined correctness checks, including transport
failures and the actual CLI restart. The
[historical-prefix proposal](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/history-prefix-design/README.md)
uses ephemeral lineage and explicitly prices the final full inventory restore.
Neither candidate has benchmark results. Their implementation preparation used
completed R5 evidence. The complete R5–R8 guard review now permits preparing the
next fresh screen; source pins and backward compatibility must be reviewed
before triggering it. No reports carry into later trials.

The [bounded response experiment](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/fetch-transport-diagnosis/README.md)
did not reproduce the retained response stalls. It also disproved a proposed
Expect-header deletion workaround with a strict interim-response client. No
production transport change was integrated from these controls.

The [R10 correctness record](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r10-history-candidate/README.md)
now covers the implementation, honest final restoration receipts and a generic
metadata-only empty receive probe. The fixed workload does not send that probe,
so its actual default-client benefit is kept separate. The
[peak-in-catalog design](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/peak-catalog-design/README.md)
proposes reducing SQL writes with explicit capability and recovery gates. Its
[implementation record](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r11-peak-candidate/README.md)
now retains independent source approval, actual fault and three-process restart
checks, and combined production-Worker historical-read/large-push verification.
Realistic 11/13-row catalog shapes with 33 refs and 11 pack descriptors retain
24,576-byte SQLite storage; first-mutation migration writes are counted.

The independent [catalog-page audit](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/catalog-pages-design/README.md)
finds a remaining potential SQL representation saving after R11. It reproduces
all 704 fresh R8 operation rows and specifies bounded pages and routing, with no
empty-page growth under churn. Its $0.056 gross projection needs actual migration,
page, failure-recovery and cursor checks. This viable next design prevents a
plateau claim; it has no implementation or benchmark result yet.

## Next declared screen: upload, historical selection and catalog peak

After complete R5–R8 guard review, this fresh batch compares provisional R8
(`0f9f62e`), R9 staged upload/publication (`8af8c48`), R10 certified historical
selection (`e30e9c3`) and R11 peak-in-catalog/recovery (`26e3d61`). Two measured
pairs per provider/profile plus warmup, four shared profile VMs, six correlated
earlier-to-later comparisons, new service state and unchanged guards are fixed
before running. Every native source tree is distinct and pinned in the workflow.
No earlier report is reused. The common reviewed Worker hash is
`f5f72b40ec605814be6a6a0311df44e255ffdc3b402afec2e66454aa5711a811`.

R8 does not negotiate staged upload; R9/R10/R11 do. Only R11 registers
`catalog_peak:1`; fresh older providers retain exact v1 catalog writes. Common
read-only schema discovery is honestly counted for every provider. Native startup
recovery precedes registration on fresh namespaces, so R11's first eligible
schema mutation belongs inside the timed first push. Actual legacy controls and
current R8/R9 contracts pass under this Worker; the independent source review
and root combined R10/R11 production-Worker contract pass. Full final validation
still uses five fresh pairs of original R1 and the selected final candidate.
