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
R2–R5, R5–R8, R8–R11 and R11–R13 batches each used four providers. Every provider gets a fresh local
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
complete source tree and index hash alongside every actual pushed source ref.
The workflow records SHA256s for every tracked Worker source module,
checks them again after timing and compares that manifest across profile VMs.
For the earlier R2–R5 batch, generation
witnesses were gated by registration capability so R2/R3 incurred no extra DO checks, and 16 MiB chunk
support still accepted old 8 MiB entries within the unchanged aggregate response limit. The completed R5–R8 batch
kept R5 scalar retirement unless negotiated. The completed R8–R11 batch kept older
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

## Candidate implementation evidence

The [R9 upload/publication record](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r9-upload-candidate/README.md)
retains clean and independent combined correctness checks, including transport
failures and the actual CLI restart. The
[historical-prefix proposal](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/history-prefix-design/README.md)
uses ephemeral lineage and explicitly prices the final full inventory restore.
Their implementation preparation used completed R5 evidence. Both were measured
in the completed R8–R11 screen below; the history classifier did not activate for
real Git clients. Its correction requires a new screen. No reports carry into
later trials.

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
plateau claim. The [reviewed implementation](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r12-catalog-pages-candidate/README.md)
now has actual mixed-format, maximum-debt, numeric-corruption, partial-failure
and same-Native recovery checks. All realistic normal/GC/reopen shapes remain
at 24,576 SQLite bytes. A combined real-Git production-Worker check passes;
the fresh R11–R13 screen below measures its net effect after actual schema work.

The next [deferred-update proposal](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/deferred-catalog-design/README.md)
would fold an all-success retirement's page update into a following upload.
Its $0.028 gross opportunity must pay for actual proof, recovery and cursor
work. Private receipts, durable replay protection and same-key pre-upload
barriers are explicit gates; observer inventory cannot certify or repair it.
The [reviewed implementation](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r13-deferred-catalog-candidate/README.md)
now passes seven final actual contracts, four Native fault cases and the combined
real-Git check. Review corrected a supported >256-key scalar-fallback wedge;
actual interrupted cleanup, reopen and GC now succeed. Normal realistic storage
remains 24,576 bytes. Those controls are separate from the fresh cost measurements below and establish no latency win.

An earlier [residual-cost audit](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/post-deferral-design-audit/README.md)
conditions its bound on separate upload/catalog-sync and publication writes.
The [reviewed atomic-coupling candidate](hosting-inline-metadata.md) changes
that persistence protocol while retaining a durable catalog/peak for every
winning manifest and syncing before retirement. Its
[local correctness record](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r14-inline-metadata-candidate/README.md)
passes realistic storage, fault, legacy and real-Git gates. It still needs a
fresh cost/performance screen. The old bound establishes no stopping result
for this new candidate and no global pack-selection optimum.

## Completed R8–R11 screen: upload, historical selection and catalog peak

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

[Run 39092442019](https://github.com/no-std-rs/git-zero/actions/runs/39092442019)
completed all four profiles and scoring at `7e7856a`. Its fresh known fixed
scores are R8 $2.05461643618, R9 $1.99701543618, R10 $1.99702143618 and R11
$1.94098593618. R9 is 2.803492% below R8; R11 is 5.530497% below R8 and
2.805954% below R10. All 216 resource and 24 per-profile cost guards pass.
R9 saves 32 Worker/DO callbacks per trial. R11 removes 56 SQL writes across
the four profiles after charging its four migrations. Storage, R2 work and
transfer sizes are unchanged; SQLite remains at 24,576 bytes.

Every comparison remains inconclusive and ineligible for plateau evidence.
R11 versus R8 fails four latency groups: 16 MiB warm large pushes, 64 MiB cold
historical fetches without haves and large pushes, and 64 MiB warm latest
fetches. R11's 136.237-second warm historical-fetch tail, versus paired Git's
0.802 seconds, remains visible. No speed win or formal promotion is established.

Both cold R10/R11 profiles perform zero historical projections, including
warmups. Git 2.53 automatically echoes the advertised `deepen-since` and
`deepen-not` capabilities, which the strict classifier omitted. The measured
full-read fallback and flat R10-versus-R9 cost are retained; actual final
observer restore GETs and read bytes are zero. A separately reviewed two-token
correction now has a real-client production-Worker regression with and without
haves. Its correctness proof does not replace this screen or establish savings.

[Published evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r8-r11-screen/README.md)
contains all five original archives, complete guards/ratios/tails, migration and
observer receipts, the activation diagnosis, and exact independent Rust offline
reproduction. After reviewing every guard, R11 is only the next provisional
known-cost comparison anchor. The corrected history path and later catalog
changes need their own fresh, prospectively pinned screen. Final validation
still requires five fresh original-R1/final pairs across all four profiles.

## Completed R11–R13 screen: corrected history and catalog updates

After the complete R8–R11 review, this fresh two-pair batch measured these
prospectively pinned providers in order:

| Label | Change | Pushed source commit | Native source tree |
| --- | --- | --- | --- |
| r11 | Provisional known-cost anchor | `26e3d61c6fecbf92a709a089ed2f1270182ae625` | `5c0a9c25198b930597e4968019bb901a19301e2f` |
| r111 (R11.1) | Real Git history capability correction | `d27848bda5f1089814839a276ee320739afcfc92` | `3b81fc646d78eedb09646bce7cdc24ea90feb313` |
| r12 | Packed catalog and numeric recovery | `134fad2b20f6e7d50ff215f08341deacbe786137` | `9c5d1a2b0036d8020a729171903b45a1b8badda2` |
| r13 | Deferred survivor update and scalar-fallback correction | `774adce8795577f4ed7f4a21c3239106abeb21e0` | `47b1a58dd852fe1492b58c739dd32d35fe529fd7` |

No earlier samples carry forward. Each provider gets new service state, one
warmup and two measured pairs on each of the four shared profile VMs. All six
comparisons reuse these freshly measured intermediate suites and are correlated.
Every comparison remains a screen; none can establish acceptance or a plateau.

The common final Worker source tree is
`22902cf4688aa7ee5da3705adceaaee0a2f08caa`, with index SHA256
`dfa5d305b6dc511cdb85eb8b7ee717d09f70e7a0435160f29cecf75126c865c1`.
The complete source manifest also freezes `catalog-pages.js` and
`retirement-proof.js`; index-only hashing cannot identify the whole Worker.
Source, runtime, reference and binary provenance checks remain mandatory.

R11/R11.1 request peak schema only. Fresh R12 requests pages but remains eager;
fresh R13 additionally negotiates deferred updates. Schema work belongs to each
first eligible timed mutation, and durable formats remain readable after
registration loss. Actual final proof/fallback contracts and source review
preserve older-client behavior. Storage, cache, body, compaction, price and
latency guards remain unchanged. The corrected cold providers must be assessed
for actual historical projection activation, with final observer restoration
work retained separately. No projected saving substitutes for a receipt.

The original scalar reference remains `556b9ecd2aa79197f86b3db2d546eee81696e899`.
After actual screen review and a substantiated stopping condition, final
validation still requires five fresh original-R1/final pairs on all profiles.

[Run 39097951585](https://github.com/no-std-rs/git-zero/actions/runs/39097951585)
completed all four profiles and scoring at `5c616c2`. Fresh known fixed
objectives are R11 **$1.94097593618**, R11.1 **$1.88455293618**, R12
**$1.83264443618** and R13 **$1.80461843618**. R13 is 7.025203% below R11
and 1.529266% below R12. All six conservative separated cost envelopes,
216 resource checks and 24 profile-cost guards pass. Actual SQLite stays
24,576 bytes and R2 peak/storage/transfer guards pass.

Historical selection now activates on real clients: each corrected cold trial
has 16 timed projections and seven full restores. GETs fall 494→425 at 16 MiB
and 654→585 at 64 MiB. Final cold observer restoration really reads another
11/13 chunks; its actual receipt remains outside the fixed workload and inside
complete physical scope. Every HEAD count is zero. SQL writes fall 86→73→66
at 16 MiB and 88→75→68 at 64 MiB, with actual first-push migrations included.

All comparisons remain inconclusive and plateau-ineligible. The detailed
comparison table retains every failed latency group. R13
versus R11 passes medians but fails four p95 groups; its worst sample is
57.956 seconds versus paired Git's 0.383 seconds. No speed or all-guard
promotion is established. All 2,816 individual ratios, 128 selected tails and
29 failed groups are retained.

[Published evidence and analysis](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r11-r13-screen/README.md)
contains the five unchanged CI archives, all actual receipt/scope tables,
source/VM/binary checks, independent 379-file inventory and byte-exact frozen
Rust reproduction of four scores and six comparisons. After complete guard
and tail review, R13 is only the next provisional known-cost anchor. Atomic
metadata and any gateway callback fusion remain unmeasured candidates. No
stopping condition is established; final validation still requires five fresh
original-R1/final pairs on every profile. Unknown cloud and Native compute
keep the full bill null.

## Failed R13–R15 attempt and fresh verification

[Run 39104016332, attempt 1](https://github.com/no-std-rs/git-zero/actions/runs/39104016332),
at `82b64319bf9d39958c861b6fb7a9a6bae25fda9b`, is invalid. Three profile jobs
completed; R14 failed during excluded 64 MiB warm warmup at push 13, before
its measured pairs. Git returned HTTP 503 and the upload-retire ProxyWorker
reported a lost connection. No root cause is established. The native stderr
remained on the destroyed VM, and the failed outer callback's billing is
unknown. Observed zero binding deltas cannot establish zero inflight work.

[Preserved original evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r13-r15-failed-attempt/README.md)
contains all four unchanged archives, ten complete reports, one incomplete
report, 1,760 available ratios, all 81 partial resource diagnostics and all
54 latency diagnostics, including every pair in 13 failed groups. Root review
independently reproduced those diagnostics. The missing two reports prevent
an all-profile cost comparison; scoring was skipped. This attempt supplies
no selection, acceptance or plateau evidence. No samples are reused below.

The reviewed [failure-only Rust runner amendment](hosting-failure-diagnostics.md)
exports stopped-host stderr tails beside incomplete reports, outside workload
timers. Successful workload logic, reference, Native providers, all five Worker
modules, rates and scoring stay fixed. The 4 MiB archive cap remains unchanged.

## Completed fresh screen: atomic metadata and first fetch batch

Fresh [run 39106495382, attempt 1](https://github.com/no-std-rs/git-zero/actions/runs/39106495382)
at `74e4b8b5cd111341043cdab04a89c047aa052cee` completed all four profiles and scoring.
Each provider was measured with two fresh pairs and an excluded warmup on each
profile VM. None of the failed attempt's samples supplied this run:


| Label | Change | Source commit | Native source tree |
| --- | --- | --- | --- |
| r13 | Provisional known-cost anchor: deferred catalog | `774adce8795577f4ed7f4a21c3239106abeb21e0` | `47b1a58dd852fe1492b58c739dd32d35fe529fd7` |
| r14 | Atomic bounded catalog/manifest metadata | `5759726b0a4545f6613553e1f2303f303eb65384` | `bfc7e5dbf7e7f244572e5d0f40e79f3190ad1707` |
| r15 | Same-gateway authority and first fetch batch | `5a7f74f58336b5474dc9920cd20857ab8e573910` | `f6c9a339159083a06531dd4982cc201ec46c13de` |

All three ran freshly on the same standard GitHub VM within each 16/64 MiB ×
cold/warm profile. Each has one excluded warmup and two measured pairs with
stock Git's MIDX bitmap and commit-graph maintenance. Every trial has 32 pushes
and 56 verified fetches. No earlier raw report or score supplies a sample.
Those providers used the common Worker source tree
`2944d3ecf512c09452e9d4487bd8bfc0b3815f91`, index SHA256
`bd42e9b9fc4039688b554c97a47d89202df4422fa5b91e008ec892811bb39f17`.
The workflow freezes all five tracked Worker modules, including inline catalog
and first-read code, before and after timing and across profile VMs.

R14's reviewed local controls preserve actual 24,576-byte realistic SQLite
layouts, durable catalog/peak publication and sync-before-retirement. R15's
final-source on/off control removes one service Worker and one DO callback per
activated cold fetch, with the same real GETs, bytes and SQL work. Its whole
fetch phase changes 24→15 Worker/DO requests, with 30 GETs/104,943,319 bytes,
13 SQL reads and no writes. These are local correctness/topology results;
the fresh measured results appear below. Legacy, boundary,
corruption, concurrent-state and ambiguous-publication controls remain separate
from performance samples.

The original scalar reference stays `556b9ecd2aa79197f86b3db2d546eee81696e899`.
The fixed objective, dated rates, strict receipts, cold-read/owning-cache/storage
limits and complete latency guards remain unchanged. Actual startup, observer
traffic and final restoration stay separately visible. Worker CPU, DO duration,
monthly SQLite retention and Native compute remain unknown; full bill is null.
Every two-pair screen is formally inconclusive and plateau-ineligible. Any
provisional cost choice requires all four profiles and every guard/tail to be
reviewed first. Final validation still requires five fresh original-R1/final
pairs on all four profiles. Prepared receiving and any socket service transport
are separate unmeasured candidates; this batch enables neither for its Native
providers.


All three CI scores and comparisons reproduce byte-exactly with the frozen
Rust evaluator. The fixed objective is the sum of all four profiles' measured
32-push/56-fetch mix repeated 1,000 times, plus each observed peak held once for
30 days. It is a known resource subtotal, with no allowances or invoice rounding.

| Provider | Known fixed objective | Change versus R13 |
| --- | ---: | ---: |
| R13 | $1.80461543618 | — |
| R14 | $1.68445743618 | −6.658372% |
| R15 | $1.64124743618 | −9.052787% |

R15 is 2.565218% below R14. All three conservative cost envelopes pass 1%,
all 108 resource guards and all 12 profile-cost guards pass. Actual cold R15
first-read consumption is 48 requests per trial on both sizes, removing 48
Worker/DO callbacks with identical GETs and bytes. Warm consumption is zero.
SQL writes per trial are 66→35→35 at 16 MiB and 68→39→39 at 64 MiB. All final
SQLite files are 24,576 bytes. First-mutation migration is included; startup
and observer operations remain separately visible, not credited as free work.

Every comparison remains **inconclusive and plateau-ineligible**: R14/R13,
R15/R13 and R15/R14 fail 6, 9 and 10 latency groups respectively, 26 limits in
total. R15's worst sample is 94.790 seconds versus paired Git's 0.995 seconds;
R13's worst is 160.642 seconds versus 0.706 seconds. No transfer-timing cause or
speed win is established. Complete review of sources, all guards and every tail
selects R15 only as the next provisional known-cost comparison anchor.

[Fresh evidence and analysis](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r13-r15-fresh-screen/README.md)
preserves all five unchanged archives, 12 reports, three scores/comparisons,
2,112 individual ratios, 96 selected tails and all 1,440 pairs in the 25 failed
latency groups. The independent verifier checks all 212 original checksums and
416 evidence hashes. Root review independently rebuilds all 24 priced trials,
all resource/profile-cost and latency guards, and every retained tail. The
failed earlier attempt remains separate evidence above. Unknown compute and
SQLite retention keep the full hosting bill null; five fresh original/final
pairs on all four profiles remain mandatory.

## Declared R15–R17 screen: small receiving and Worker service sockets

After the complete fresh R13–R15 review, a new three-provider screen measures:

| Label | Change | Exact source commit | Native source tree |
| --- | --- | --- | --- |
| r15 | Provisional known-cost anchor: first fetch batch | `5a7f74f58336b5474dc9920cd20857ab8e573910` | `f6c9a339159083a06531dd4982cc201ec46c13de` |
| r16 | First verified Full reconstruction for small receiving | `603b224eda3719c7525db00ac6a112a71368d7b3` | `a255cf03e52648248cd0e71051a3c2a9d8e0a827` |
| r17 | Bounded Native-to-Worker service socket transport | `c128197cc2cf1761e93832c63eec4c25c1268249` | `328a5bd64b51e2335d5aa3432d46c07d1d022655` |

Each profile gets a fresh standard GitHub VM, with these providers measured
sequentially using separate Worker processes and state, two pairs each and an
excluded warmup. Every comparison uses only this new attempt's reports; the
three earlier-to-later comparisons share intermediate suites and are correlated.
The original scalar Git wrapper, Git 2.53 policy, objective, dated prices,
32 MiB/256-entry owning cache, storage caps and all guards stay fixed.

The common Worker source is `7f41db4407963ed04e6cbe19556b1416633caf18`, with
index SHA256 `0b3183bb89d7920c9f1d0868895ec13414d74e74e096f8181b30654e418c0bbd`.
All six source modules, including `service-socket.js`, are frozen before/after
measurement and across VMs. R15/R16 do not negotiate sockets; the shared ordinary
HTTP bridge is independently verified equivalent to their preceding Worker.

The [R16 candidate evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r16-small-receive-candidate/README.md)
proves eight same-source small/thin receiving pairs each remove one Worker/DO
callback, with identical Full GETs, bytes, writes and verified Git contents.
Its shared error cleanup, one-use Full token, unknown receipts, ambiguity and
same-Native GC-before-PUT controls remain separate from performance samples.

The [R17 candidate evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r17-worker-socket-rpc-candidate/README.md)
proves an initial Upgrade is one Worker request and later messages dispatch
ordinary DO HTTP calls, each still one DO request. There is no DO-socket billing
discount. Connections rotate after 16 RPCs; every Upgrade is charged. Actual
maximum payloads, strict framing/authentication/ACK/session controls and the
256 KiB retained reservation inside the same shared cache cap passed review.
Exact-source Git on/off and 29 fault controls verify no automatic resend after
ambiguous publication and recovery before a later PUT. Those are correctness
and topology controls; only this new screen can measure net cost and latency.

The protected immutable-chunk cache is a separate unmeasured candidate and is
not enabled by these three source pins. No stopping condition is established.
All two-pair comparisons remain formally inconclusive; final validation still
requires five fresh original-R1/final pairs on every profile, with full unknown
cloud and Native compute left unknown.

## Completed screen: small receiving and Worker service sockets

[Run 39109536794](https://github.com/no-std-rs/git-zero/actions/runs/39109536794),
at exact head `becec3f57b74ae19c9251ad3ddea25e62de07c84`, completed all four
profiles and scoring. These are fresh two-pair measurements of the three source
pins declared above; no sample from an earlier attempt is included.

The known fixed objective uses the same four-profile 32-push/56-fetch mix,
1,000 repetitions and one observed peak held for 30 days:

| Provider | Known fixed objective | Change versus R15 |
| --- | ---: | ---: |
| R15 | $1.64123743618 | — |
| R16 | $1.61963943618 | −1.315958% |
| R17 | $1.49963343618 | −8.627880% |

R17 is 7.409427% below R16. All three conservative cost envelopes pass 1%,
all 108 resource guards and all 12 profile-cost guards pass. Cold small receiving
consumes 24 preparations per trial, removing 24 Worker/DO callbacks without
changing GETs, bytes, PUTs, deletions or SQL writes. Warm preparation remains off.

Per-trial Worker/DO requests are 319/319→295/295→184/295 for cold16,
444/444→420/420→192/420 for cold64, 208/208→208/208→178/208 for warm16,
and 210/210→210/210→179/210 for warm64. Socket messages retain their ordinary
DO HTTP charge. Every actual Upgrade, including rotations, is counted; ACKs and
physical frames remain separate diagnostics. Cold GETs/read bytes stay 425 /
1,771,600,643B and 585 / 3,831,138,627B, respectively; warm payload reads stay zero.
SQL writes remain 35/39, and every final SQLite file is 24,576 B. The 256 KiB socket
reservation counts inside the unchanged 32 MiB owning cache budget.

All comparisons remain **inconclusive and plateau-ineligible**. R16/R15,
R17/R15 and R17/R16 fail 8, 10 and 10 latency groups respectively: 28 groups and 43
limits overall. R16's worst operation is 161.428 seconds versus paired Git's
0.789 seconds; R17's is 68.531 seconds versus 0.408 seconds. Every slow operation
is retained; no cause, uniform speed win, formal promotion or stopping follows.

[Fresh evidence and analysis](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r15-r17-fresh-screen/README.md) preserves all five unchanged original archives
(5,099,721 B), 12 reports, three scores/comparisons, 2,112 individual ratios, 96
selected tails and all 1,664 baseline/candidate pairs in the failed latency
groups. Root review independently rebuilds all 24 priced trials, resource and
latency guards, every tail selection, and startup/fixed-scope arithmetic.
Startup adds an observed known subtotal of $0.045628 across the four-profile
objective, separately visible. Worker CPU, DO duration, monthly SQLite retention
and Native hosting remain unknown, so the full bill stays null. Final validation
still requires five fresh original-R1/final pairs on all four profiles.

After complete source, receipt-scope, guard and tail review, R17 is only the
provisional known-cost anchor for the next fresh comparison. The R18 protected
chunk cache is source-approved separately; its ledger is independently approved and the fresh screen is declared below. The bounded multi-chunk design is being implemented in isolation. No
stopping condition is established.

## Declared R17–R18 screen: one protected verified chunk

After the complete R15–R17 source, receipt-scope, guard and tail review, a fresh
screen compares the provisional R17 cost anchor with the source-reviewed R18
cache. Each of the four profiles uses a fresh standard GitHub VM, separate
provider processes and state, two measured pairs, and an excluded warmup.

| Label | Change | Exact source commit | Native source tree |
| --- | --- | --- | --- |
| r17 | Provisional known-cost anchor: service socket reuse | `c128197cc2cf1761e93832c63eec4c25c1268249` | `328a5bd64b51e2335d5aa3432d46c07d1d022655` |
| r18 | One protected immutable chunk admitted from a required verified GET | `f1ba8f548507dca1e5ab78e5c5d20d8360ecee21` | `c5b06db80540cc21a80e1ad19b4b8d1086425749` |

The common six-module Worker remains `7f41db4407963ed04e6cbe19556b1416633caf18`,
with index SHA256 `0b3183bb89d7920c9f1d0868895ec13414d74e74e096f8181b30654e418c0bbd`.
Whole-source before/after and cross-profile checks stay enabled. The Rust runner,
original scalar reference, Git 2.53 maintenance policy, dated price card, workload,
cache and storage caps, two-pair stage, and all resource/latency guards are fixed.

R18 retains at most one independently verified chunk of at most 16 MiB, within
the same joint 32 MiB/256-entry budget as responses, metadata, certificates and
the 256 KiB service socket reservation. Admission occurs only after a required
ordinary request GET verifies digest and length. Startup, observers, PUT and
warm preloading do not populate it. Exact ordered resolution merges the retained
chunk with all required missing chunks before materialization. Full-receive
validation survives its own admission; a stale cache mask causes a counted
prefetch discard and ordinary safe reconstruction. New published chunks remain
uncached, so cold latest-after-push reads still make naturally required GETs.

[Candidate evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r18-protected-chunk-candidate/README.md)
separates correctness controls from these new measurements. Local matched
controls, full-size ownership and response pressure, an actual in-flight eviction,
true thin receiving, ambiguity/GC, unknown receipts and warm/HTTP compatibility
passed independent source and ledger review. These controls establish no net
cost or speed result. Actual GETs, bytes, admissions, evictions, joint ownership,
all socket Upgrades/RPCs, startup, observer work and slow fetches remain visible.

R19's bounded multi-chunk pool is being implemented separately and is absent
from both source pins. No stopping condition is established. The screen remains
formally inconclusive and plateau-ineligible; five fresh original-R1/final pairs
on all four profiles are still mandatory, and unknown compute keeps the full
hosting bill null.

## Completed screen: one protected verified chunk

[Run 39113545149](https://github.com/no-std-rs/git-zero/actions/runs/39113545149),
at exact head `556fa3bcacb46bf7cf1f7a3c04c6b3db77c54667`, completed all four
profiles and scoring. All eight reports are fresh two-pair measurements; no
sample or score from an earlier run is reused.

| Provider | Known fixed objective | Change versus R17 |
| --- | ---: | ---: |
| R17 | $1.49964143618 | — |
| R18 | $1.42325543618 | −5.093618% |

This uses the same four-profile 32-push/56-fetch mix, 1,000 repetitions and one
observed peak held for 30 days. All 36 resource guards, four profile-cost guards
and the conservative separated 1% cost envelope pass. The $0.076386 saving
comes from actual trial means: 158 fewer R2 GETs ($0.05688), 116 fewer DO calls
($0.0174), seven fewer Worker requests ($0.0021), and six fewer SQL read rows
($0.000006). Writes, physical storage peaks and their known prices are unchanged.
The preceding R17 figure differs slightly from its earlier fresh screen because
observed SQL read rows vary; comparisons here use this attempt alone.

Cold16 GETs/read bytes fall 425 / 1,771,600,643 B to 346 / 1,096,568,553 B;
cold64 falls 585 / 3,831,138,627 B to 506 / 2,505,738,563 B. Each retains one
verified payload, 8,544,710 B or 16,777,216 B, with 79 timed hits and no evictions.
First admission occurs at the first latest fetch. Warm payload GETs, admissions
and hits remain zero. Every cold latest-after-push fetch still has a naturally
required GET. The final observer uses one existing cached chunk separately and
admits none. Preparation Unsupported paths, ordinary fallback and every paid
socket Upgrade remain counted. The maximum observed joint owning gauge is
29,857,423 B and source-exported retained peak 29,860,139 B, below the same
32 MiB/256-entry limit including metadata, certificates and service sockets.

The comparison remains **inconclusive and plateau-ineligible**: five latency
groups fail five limits. R18's worst operation is 173.087 seconds versus paired
Git's 0.952 seconds; R17's worst is 61.078 seconds versus 0.407 seconds. No
cause, uniform speed improvement, formal promotion or stopping follows.

[Fresh evidence and analysis](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r17-r18-fresh-screen/README.md)
retains all five original archives (3,546,008 B), eight reports, both scores,
the comparison, all 1,408 individual ratios, 64 selected tails and all 160
individual pairs in the five failed latency groups. All 157 original checksums
and 364 evidence hashes pass; the frozen Rust evaluator reproduces both scores
and the comparison byte for byte. Root independently rebuilds all 16 priced
trials, guards, tail selections, failed pairs, scope arithmetic and saving
decomposition. Startup's known $0.045628 four-profile subtotal remains separate.
Worker CPU, DO duration, monthly SQLite retention and Native hosting stay
unknown, so the full bill remains null.

After complete source, scope, guard and tail review, R18 is only the provisional
known-cost comparison anchor. The R19 multi-chunk source is committed separately
and under final independent review; it is absent from both measured source pins.
Its optional admission headroom and the fresh-schema design remain unmeasured
follow-ups. No stopping condition is established, and five fresh original-R1/final
pairs on all four profiles are still mandatory.


## Declared R18–R19 screen: bounded secondary chunks

After complete review of fresh R17–R18 sources, scopes, guards and tails, a new
two-provider screen compares the provisional R18 cost anchor with R19. Each of
the four profiles uses a fresh standard GitHub VM, separate provider processes
and state, two measured pairs, and an excluded warmup. No older samples enter
this comparison.

| Label | Change | Exact source commit | Native source tree |
| --- | --- | --- | --- |
| r18 | Provisional known-cost anchor: one verified chunk | `f1ba8f548507dca1e5ab78e5c5d20d8360ecee21` | `c5b06db80540cc21a80e1ad19b4b8d1086425749` |
| r19 | Bounded first-fit secondary chunks and confirmed pruning | `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5` | `521d427a056711a58b757c9e52f3d5e047937f92` |

The common six-module Worker stays `7f41db4407963ed04e6cbe19556b1416633caf18`,
with index SHA256 `0b3183bb89d7920c9f1d0868895ec13414d74e74e096f8181b30654e418c0bbd`.
Whole-source before/after and cross-profile checks, the Rust runner, original
scalar reference, Git 2.53 preparation policy, dated prices, workload, storage
limits and all resource/latency guards stay fixed.

R19 can retain up to 32 verified chunks within the same joint 32 MiB/256-entry
budget. It considers every already-required verified miss in selection order and
never evicts a live owner to admit a later scan result. Beyond the first anchor,
admission also limits total chunk owning bytes to 17 MiB, leaving 15 MiB
unallocated as a response/control preference. That space is not charged as owned
memory and does not cap responses at 14 MiB. Actual required memory pressure
evicts newest secondary chunks, then the anchor, before response LRU entries;
eligible responses up to 16 MiB remain retainable. Keys, headers, pool capacity
and growth overlap, actual metadata and the 256 KiB socket reservation count.

Pruning requires a trusted current Full manifest after confirmed publication or
ordinary authoritative recovery. Proposed/stale/ambiguous manifests, projection
views and observers do not prune. Logical pruning is distinct from physical
retirement. Startup, observers, PUT and prewarming never admit chunks. Cold
latest reads retain naturally required new-chunk GETs. Stable Full-receive tokens,
exact ordered missing/cached resolution, paid discarded prefetch, unknown receipt
states and same-Native physical GC before another upload remain enforced.

[Candidate evidence](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r19-multi-chunk-candidate/README.md)
separates local correctness controls from these new measurements. Same-source
16/64 MiB ONE/MULTI controls, real response/allocator pressure, 38 fault rows,
in-flight mask eviction, warm/HTTP and same-directory restart passed independent
source and ledger review. Startup work in copied-fixture controls remains paid
and separate from ordinary warm reads. Local counts and conditional replay are
not the measured net saving of this fixed workload.

The new joint-only admission proposal and fresh-schema bootstrap remain separate
unmeasured designs and are absent from both source pins. The screen is formally
inconclusive and plateau-ineligible. No stopping condition is established; five
fresh original-R1/final pairs across all four profiles remain mandatory. Unknown
cloud and Native compute keep the full hosting bill null.


## Completed screen: bounded secondary chunks

Fresh [run 39118509165](https://github.com/no-std-rs/git-zero/actions/runs/39118509165)
measured approved R18 and R19 on four fresh standard GitHub VMs, with two pairs
and one excluded warmup per profile. Both providers used the same six-module
Worker, common Rust runner/scorer and prepared Git 2.53 reference. All four
profiles, the score job and all eleven required CI checks passed.

| Provider | Known fixed objective | Change from R18 |
| --- | ---: | ---: |
| R18: one verified chunk | $1.42325343618 | — |
| R19: bounded verified chunk pool | $1.29245743618 | −9.1899% |

This objective repeats each 32-push/56-fetch profile 1,000 times and prices its
observed peak storage for 30 days under the frozen rate card. It is a known
subtotal: Worker CPU, DO duration, monthly SQLite storage and Native compute
remain unknown, so the full hosting bill stays null. Actual startup and observer
work remain separately visible; startup contributes $0.011407 per profile when
priced at the same repeat multiplier, outside the fixed workload.

Across the cold-profile means, R19 avoids 345 GETs ($0.1242), 38 DO dispatches
($0.0057) and three Worker requests ($0.0009), while four additional SQL rows
read cost $0.000004. The net saving is $0.130796. It is the measured result,
rather than the earlier conditional 338-GET replay.

| Cold profile, each trial | R18 | R19 |
| --- | ---: | ---: |
| 16 MiB GETs | 346 | 153 |
| 16 MiB R2 bytes read | 1,096,568,553 | 569,108,040 |
| 64 MiB GETs | 506 | 354 |
| 64 MiB R2 bytes read | 2,505,738,563 | 2,449,962,286 |

Warm ordinary reads still use zero R2 GETs. Written bytes, peak/live storage,
SQL writes and SQLite footprint remain equal; all 36 resource guards, four
profile-cost guards and the conservative 1% cost envelope pass. Actual pool
admission, pruning and rejection vary by profile. The maximum observed joint
gauge is 30,683,668 bytes, with source-reported peak 30,686,434, under the same
32 MiB/256-entry budget. The later-admission 17 MiB preference remains
unallocated space, rather than fictitious owned memory or a response-size cap.
Observer hits do not become new admissions or pruning. Naturally new cold
latest reads still need real GETs.

All 24 median latency guards pass, but three p95 guards fail: cold16 large
pushes, cold16 history with haves, and warm16 history without haves. The largest
R19 elapsed operation remains a cold64 historical fetch at 141,599.712 ms,
versus 794.698 ms for its paired Git operation. These tails remain in the
results. The screen is **inconclusive and plateau-ineligible**; there is no
general speed win or stopping conclusion.

[Fresh evidence and analysis](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r18-r19-fresh-screen/README.md)
preserve all five original archives, frozen byte-exact score reproduction,
source/runtime identities, request scopes, all 1,408 ratios, 64 selected tails
and all 96 individual pairs in failed latency groups. Root independently
reconstructed all 16 priced trials, 36 resource/four cost guards, 24 latency
groups, startup/observer scope algebra, selected tails and publication bytes.
After this complete review, R19 is only the provisional known-cost comparison
anchor. Five fresh original-R1/final pairs across all four profiles remain
mandatory.

The isolated Joint-only admission candidate completed local correctness
validation. Its first16 pair reduces actual GETs with unchanged mutation and
storage totals; one response has proven identical decoded objects with a
17-byte pack-encoding difference. The clean64 pair also passes: 375→282 ordinary
GETs with identical mutations/storage and supported read/receive preparation.
Its one 45-byte response variance has exact decoded-object proof. A first64
attempt failed during an observer request and is excluded, with missing
error-body evidence disclosed. A separate traced pair exercised fallback only;
it does not establish optimized-path performance. Correctness controls use
separately scoped volatile temporary storage; their timings are not performance
measurements. Exact Joint source `b74c917` and all six bounded controls have
passed independent review. The [corrected ledger](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r20-joint-chunk-candidate/ledger-review-final.md)
also passed independent review; the fresh priced screen follows below. Its
bounded small synthetic wire samples are explicitly disclosed alongside the
local-only large payloads and state.

The next [response-retention design](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/next-candidate-designs/fit-response-independent-design-review.md)
would skip optional upload response retention when it would evict existing
owners. Its conditional replay saves 70 GETs ($0.0252); a larger-pressure
counterexample regresses three GETs. Actual ownership and a fresh screen must
settle the result. Exact source `95fdf58` has now passed final independent
correctness review, including allocator, fault, restart, warm and HTTP controls.
Its ledger publication and fresh R20/R21 priced screen remain pending. The
fresh-schema proposal has isolated implementation authorization for focused
tests and a cheap actual 16/64 MiB startup/first-push proof. Neither change
enters this completed screen.

A [read-only fetch phase audit](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/fetch-latency-audit/AUDIT.md)
shows that the codec elapsed counter excludes view preparation and response
delivery. Several warm tails have zero R2 reads, so storage reads do not explain
every slow request. The cause remains unresolved. A separate small Rust
diagnostic proof has independently approved phase records, including actual
failed HTTP bodies and partial-write stages. Its first runner had a cooperative
deadline; a [separate hard supervisor](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/fetch-latency-audit/stage2-independent-review.md)
has passed independent review and tiny stall/cleanup controls. The bounded
executor and corrected tiny same-path proof have also passed independent review.
Every newly fetched pack receives explicit strict indexing, and resource helpers
run inside its supervised budget. Both large cases have now completed once:
cold64's selected fetch took 4,698.999 ms and warm16's 103.216 ms; no operation
reached ten seconds. The historical tails did not reproduce. These runs used
different source and temporary storage from the historical GitHub VM evidence;
they establish no cause, fix or speed result. Cold actual environment capture
is incomplete. The [independent result review](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/fetch-latency-audit/reviewed-batch02/independent-REVIEW.md)
verifies the retained execution with that material limit. A capture/portability
correction is being prepared for a separate standard-VM disk replay. Diagnostic
timings cannot replace the unchanged benchmark.

## Declared R19/R20 screen

The declared screen compares approved Joint source with the provisional R19
known-cost anchor. Source and corrected-ledger reviews are complete; the workflow
pins only these two providers. Its fresh results follow below.

| Provider | Exact source commit | Native source tree |
| --- | --- | --- |
| R19 | `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5` | `521d427a056711a58b757c9e52f3d5e047937f92` |
| R20 | `b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0` | `ff99eaf58a5e4f68e744d0b50ddee2af67aff1cf` |

Both providers use the same six Worker modules, source tree
`7f41db4407963ed04e6cbe19556b1416633caf18`, frozen rates and Rust runner/scorer
logic. Each fresh VM measures both providers sequentially for one of the four
16/64 MiB cold/warm profiles. Two pairs per provider retain all 32 pushes and
56 interleaved fetches, their paired tuned-Git operations, separately charged
startup/observer scopes, and every latency tail.

The objective, 1% resource/profile-cost limits, conservative cost separation,
Git-normalized latency guards and artifact limits remain unchanged. All four
profiles must be reviewed before selecting another provisional cost anchor.
This screen cannot establish a plateau. The separate response-retention
implementation and diagnostic executor are excluded. Five fresh original-R1
versus final pairs across all four profiles remain required.

## Completed R19/R20 screen

Fresh [run 39128927970](https://github.com/no-std-rs/git-zero/actions/runs/39128927970)
at `d5cdefe` completed all four profiles and scoring. The frozen Rust evaluator
reproduces both scores and their comparison byte for byte. This objective repeats
each profile's 32 pushes and 56 fetches 1,000 times, retaining observed R2 peak
storage once for 30 days; unknown compute keeps the full bill null.

| Provider | Known subtotal | Change |
| --- | ---: | ---: |
| R19 | $1.29245543618 | baseline |
| R20: joint verified-chunk ownership | $1.21247143618 | −6.1885% |

Cold16 GETs fall 153→85 and bytes read 569,108,040→219,786,498. Cold64 GETs fall
354→229 and bytes read 2,449,962,286→1,849,065,488. Warm payload GETs remain zero.
Across the four profile means, 193 fewer GETs, 62 fewer DO dispatches, four fewer
Worker requests and four fewer SQL rows read account for the $0.079984 saving.
All named physical writes, live/peak storage, SQLite and compaction observations
agree for all 1,056 paired operations, including warmups. All 36 resource and
four profile-cost guards pass.

All 24 paired Git-normalized median guards pass, but five p95 groups fail:
warm16 historical reads without haves and latest rechecks; cold64 historical
reads with haves; warm64 latest and historical reads without haves. R20's worst
fetch is cold64 `history_no_haves_24`, sample 1: 159,373.757 ms against Git's
1,095.949 ms. These failures remain in the results. The formal screen is
**inconclusive and plateau-ineligible**; it establishes no speed win or stopping
condition.

The [evidence publication](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r19-r20-fresh-screen/README.md)
contains all five exact original archives, source/runtime identities, separate
startup/observer scopes, every ratio and tail, and all failed-group pairs. Root
independently reconstructed pricing, 156 source identities, 162,888 service-scope
checks, 2,112 cache states and all guards/tails/publication bytes. Actual retained
cache ownership peaks at 31,048,069 bytes within 32 MiB. Joint incurs 28/29
pressure evictions per cold trial versus Multi's zero; it is not a resident
superset.

After complete review, the [coordinator decision](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/ci-r19-r20-fresh-screen/ROOT-REVIEW.md)
selects R20 only as the provisional known-cost comparison anchor. Response
retention, fresh-schema bootstrap and phase diagnostics do not enter these
measurements. Five fresh original-R1/final pairs on every profile remain
mandatory and have not run.

The [actual R20 residual-cost audit](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/next-candidate-designs/after-joint-actual-cost-audit.md)
finds no further callback-grouping credit under unchanged bytes and bounds.
The [initial-container revision](benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/next-candidate-designs/initial-container-revision-1.md)
rejects its fully journaled transparent legacy-writer variant: at least $0.050
extra SQL writes exceed the $0.005275 remaining margin. A separate versioned
new-Native-only writer contract remains unresolved; no global minimum follows.
No container implementation or runtime was authorized.
