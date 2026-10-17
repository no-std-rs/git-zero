# Local hosting cost optimization experiments

This experiment optimizes known resource dollars using actual local workerd R2 and SQLite Durable Object
operations. It uses the Rust `hosting-bench` runner and offline Rust `hosting-cost` evaluator. Nothing is
deployed and no paid Cloudflare resources are used. See [the runtime instructions](../examples/repository-host/cloudflare/README.md)
and [the existing measurements](hosting-cloudflare-benchmarks.md) for topology and accounting details.

The active workload is [interleaved large pushes and historical fetches](hosting-interleaved-benchmarks.md).
The fixed 32:16 objective, reproduction commands and measured tables below retain the original batch
experiment. They must not be compared numerically with the new 32:56 interleaved objective.

## Fixed objective and acceptance

The objective is the equal-weight sum of four profiles: 4 MiB cold, 4 MiB warm, 16 MiB cold and 16 MiB warm.
Each profile repeats exactly 32 measured pushes and 16 distinct varied fetches 1,000 times, and retains one
observed R2 peak for 30 days. Score gross linear known resource charges before account allowances,
subscription minimums and invoice rounding. Whole-trace costs, including clone, probe publication and
verification operations, are reported separately so auxiliary traffic remains visible.

An accepted candidate must satisfy all of these conditions against the last accepted state:

- At least 1% lower aggregate known cost, with candidate maximum trial cost at least 1% below baseline
  minimum trial cost. Faster elapsed time alone does not improve the objective.
- No profile's mean known cost or measured resource guard grows by more than 1%: R2 peak/live bytes,
  R2 read/written bytes, current SQLite bytes and varied-fetch transfer bytes. Keep the 32 MiB/256-entry
  response cache and repository limits fixed and enforce their existing caps.
- For each measured workload, paired host/Git latency ratios stay within baseline median +10% and p95 +25%.
  A breach is inconclusive and requires an independent quiet-window rerun; retain every original tail.
- Complete operation receipts, consistent Worker accounting epochs and observer-excluded counters; valid
  refs, trees, archives and strict fsck; nonempty packs for every varied fetch; unchanged authoritative
  read-phase inventory with no R2 PUT/DELETE; positive actual R2 reads for cold profiles.

Unknown Worker CPU, Durable Object duration, monthly SQLite storage and native codec compute stay unknown.
Client or emulator elapsed time must never substitute for billed cloud compute. The score is a known
subtotal; `total_estimate_usd` remains null while billing dimensions are unmeasured.

## Roles and measurement sequence

The coordinator owns commits, pushes, service lifecycle and the final decision. An implementation agent
changes one design at a time in a separate worktree. The measurement agent runs the frozen committed build
and records evidence independently. The evaluator validates counters and applies the fixed objective.
During timing, implementation may edit source in its separate worktree, but no builds, tests or second
benchmark run may compete with the experiment.

1. Test and build before timing. Commit and push the baseline or candidate before measuring it. Freeze
   source and binaries; record commit and remote SHA, candidate parent, both executable SHA-256 hashes,
   Worker/config/lockfile hashes, runtime versions, emulator port/PID/cwd and state/log paths.
2. Use a dedicated local emulator with fresh state for the frozen Worker source. Do not reuse an unrelated
   checkout's watcher or stop its services. Confirm local bindings and `/health`, then wait for a quiet CPU
   window. Record contention before and after trials; do not silently discard contention or long tails.
3. Establish a fresh baseline with all four profiles and five paired samples each. One warmup per backend
   is excluded; the runner alternates Cloudflare/Git order and creates fresh repository namespaces.
4. Screen a candidate with two paired samples at 16 MiB cold/warm. Measure affected 4 MiB profiles when
   promising. Partial screens guide investigation and cannot accept a candidate or declare a plateau.
5. Validate a promising candidate with all four profiles and five paired samples. Compare only matching
   configuration, fixture refs/probes, machine/runtime and sample count. Reverse profile order between
   states when practical. Preserve raw reports and their file hashes alongside the score and comparison.
6. Record every accepted, rejected or inconclusive attempt. Resume from the last accepted state. Stop after
   two independent viable designs are rejected for failing the 1% cost threshold; incomplete, incomparable,
   resource-regressing or latency-inconclusive runs do not count. An independently substantiated lower
   bound showing remaining cost is required I/O or unpriced compute may also justify stopping.

Historical reports remain historical evidence until a fresh run reproduces the frozen baseline. Changes
to workload weights, pricing, cache policy or limits define a new experiment rather than an improvement.

## Reproduce a full state

Build and correctness checks must finish before starting the emulator and this sequential measurement loop.
Start the dedicated emulator using the runtime instructions, with its frozen worktree's config, port 8808
and an experiment-specific `--persist-to` path. Run from that worktree:

```sh
mkdir -p /tmp/git-zero-cost-baseline
for mib in 4 16; do
  for view in cold warm; do
    target/hosting/hosting-bench \
      --cloudflare-endpoint http://127.0.0.1:8808 \
      --cloudflare-token git-zero-local-services --cloudflare-repo-prefix costbaseline \
      --view-cache "$view" --samples 5 --fixture-mib "$mib" \
      --pushes 32 --random-fetches 16 --reference-maintenance bitmap --monthly-runs 1000 \
      --output "/tmp/git-zero-cost-baseline/${mib}mib-${view}.json" || break 2
  done
done
target/hosting/hosting-cost --output /tmp/git-zero-cost-baseline/score.json \
  /tmp/git-zero-cost-baseline/4mib-cold.json /tmp/git-zero-cost-baseline/4mib-warm.json \
  /tmp/git-zero-cost-baseline/16mib-cold.json /tmp/git-zero-cost-baseline/16mib-warm.json
```

Repeat the runner commands for the candidate with its own namespace prefix, output directory and frozen
runtime. Then compare the four complete candidate reports:

```sh
target/hosting/hosting-cost --baseline /tmp/git-zero-cost-baseline/score.json \
  --output /tmp/git-zero-cost-candidate/comparison.json \
  /tmp/git-zero-cost-candidate/4mib-cold.json /tmp/git-zero-cost-candidate/4mib-warm.json \
  /tmp/git-zero-cost-candidate/16mib-cold.json /tmp/git-zero-cost-candidate/16mib-warm.json
```

The CLI fails closed on invalid or incomplete inputs. A successfully written comparison still requires
inspection of `comparison.decision` and its reasons; successful process exit alone does not accept a change.

## Results ledger

The baseline and first candidate below are fresh. Cost reductions and latency acceptance are evaluated separately; no candidate is accepted while a declared guard is inconclusive.

| State/design | Commit / pushed SHA | Paired samples | Actual counts and score | Decision/evidence |
| --- | --- | --- | --- | --- |
| Fresh baseline | `9beaa1ee5021a2fdf942c3b95509547445985d76`, pushed before measurement | 5 per profile; one excluded warmup per backend | $3.1200383036 known monthly fixed-workload objective | All four profiles complete; Rust evaluator passed; latency limitations below |
| Bundle pack and index in immutable chunks, preserving legacy reads | `556b9ecd2aa79197f86b3db2d546eee81696e899`, pushed before measurement | 5 per profile; one excluded warmup per backend | $1.8241739036, **41.53% lower**; all resource and profile cost guards pass | **Inconclusive:** 16 latency guards fail; investigate transport delays and independently rerun; does not count toward plateau |

Baseline reports: [4 MiB cold](benchmarks/hosting-cost-loop-2026-10-18/baseline/4mib-cold.json.gz),
[4 MiB warm](benchmarks/hosting-cost-loop-2026-10-18/baseline/4mib-warm.json.gz),
[16 MiB cold](benchmarks/hosting-cost-loop-2026-10-18/baseline/16mib-cold.json.gz),
[16 MiB warm](benchmarks/hosting-cost-loop-2026-10-18/baseline/16mib-warm.json.gz),
[score](benchmarks/hosting-cost-loop-2026-10-18/baseline/score.json) and
[provenance](benchmarks/hosting-cost-loop-2026-10-18/baseline/provenance.json).
The sum of trial-minimum scores is $3.1200379036 and trial maxima sum to $3.1200399036. The small spread
comes from measured SQLite reads; allowances and rounding are excluded. These are known subtotals, with
Worker CPU, Durable Object duration and monthly SQLite storage still unmeasured; native compute is outside
the model.

| Profile | Known fixed-workload USD | Fixed-workload R2 A / B | Worker / DO requests | R2 peak bytes |
| --- | ---: | ---: | ---: | ---: |
| 4 MiB cold | 0.8715730852 | 66 / 332 | 678 / 582 | 4,739,013 |
| 4 MiB warm | 0.6026530852 | 66 / 0 | 346 / 250 | 4,739,013 |
| 16 MiB cold | 1.0428208666 | 67 / 543 | 884 / 788 | 11,191,107 |
| 16 MiB warm | 0.6029912666 | 67 / 0 | 341 / 245 | 11,191,107 |

Each profile passed content validation, complete observer-excluded accounting, cold-read checks and
unchanged authoritative read-phase inventory. Worker and native hashes matched the frozen provenance, and
the source worktree stayed clean. No experiment builds or tests ran during timing. External workloads
caused substantial and changing contention: initial CPU pressure avg10 was 67.04% and load average 108.78
on 24 logical CPUs. Timing tails are retained, including a 25,210.71 ms 16 MiB warm initial push and
7,606.71 ms 4 MiB cold warm clone. The 4 MiB warm initial-push median was 6,776.04 ms despite low surrounding
CPU pressure in later trials; recorded native receive stages do not explain those delays. These traces
support actual operation costs but cannot establish production latency or attribute every stall to CPU
contention. Candidate acceptance still requires the predefined latency guards and independent reruns when
they fail.

For each measured row, retain baseline/candidate IDs, raw-report and score paths/hashes, per-profile actual
R2/Worker/DO/SQL counts, known-dollar reduction, storage and latency guard results, missing billing dimensions
and the reason for acceptance/rejection. Record reruns as additional evidence rather than replacing failures.

### Round 1: bundled pack and index

The pack and index now share the same content-addressed 8 MiB chunk stream. Legacy manifests remain readable.
Small packs use one R2 key instead of two; exact compressed bytes, cache budgets and publication ordering are
preserved. Five paired trials per profile completed, with the unchanged benchmark, cost evaluator and Worker.
The candidate cost envelope is $1.8241739036–$1.8241739036, below the baseline envelope. All storage, transferred
byte and per-profile cost guards passed. This is a known resource saving, not an accepted latency improvement.

| Profile | Known fixed-workload USD | Fixed-workload R2 A / B | Worker / DO requests |
| --- | ---: | ---: | ---: |
| 4 MiB cold | 0.4951020852 | 33 / 166 | 451 / 355 |
| 4 MiB warm | 0.3606420852 | 33 / 0 | 285 / 189 |
| 16 MiB cold | 0.6040948666 | 34 / 296 | 579 / 483 |
| 16 MiB warm | 0.3643348666 | 34 / 0 | 283 / 187 |

[Comparison](benchmarks/hosting-cost-loop-2026-10-18/r1-bundle/comparison.json),
[summary](benchmarks/hosting-cost-loop-2026-10-18/r1-bundle/summary.json),
[latency guard details](benchmarks/hosting-cost-loop-2026-10-18/r1-bundle/latency-guard-failures.json),
[log diagnosis](benchmarks/hosting-cost-loop-2026-10-18/r1-bundle/latency-diagnostic.md) and
[provenance](benchmarks/hosting-cost-loop-2026-10-18/r1-bundle/provenance.json) retain the measured outcome.
Each artifact directory includes compressed raw reports, `SHA256SUMS` for published files and
`RAW_SHA256SUMS` for the original uncompressed reports. No timing samples are removed.

The 6.7-second initial-push tails are localized to outer receive POSTs: advertisements took 2–4 ms,
nearby service RPCs took 1–50 ms, and native measured phases totaled 60–83 ms. Existing logs cannot
separate HTTP body intake, Worker forwarding and scheduling. A separate 7.8-second append stall is
localized to a slow bundle PUT. These observations do not justify attributing every delay to one cause.
The [isolated transport diagnosis](benchmarks/hosting-cost-loop-2026-10-18/transport-diagnosis/REPORT.md) reproduces the body-intake stall without Git, R2 or SQLite. Direct uploads were fast. A fresh buffered-versus-streaming control did not reproduce it, so neither buffering nor concurrent accept is an evidenced fix. The original guard failures remain. Further transport changes need correlated client and Rust HTTP-stage evidence.

### Round 2 design: bounded snapshot read batching

An independent design agent audited the R1 receipts. The fixed cold workloads perform 166 and 296 R2
GETs across 48 and 49 nonempty snapshot rehydrates. Batching each snapshot within a 16 MiB framed response
is projected to remove 365 Worker and DO bridge requests while preserving the actual R2 calls and bytes.
The projected aggregate is $1.6599239036, 9.004% below R1. This estimate is not a measured result or an
acceptance decision. A clean implementation agent is adding bounded binary framing, scalar defaults at
the service boundary, full length/hash checks, legacy fallback and unchanged per-binding receipts.

R2 implementation is committed and pushed as `b0a24bce5877440d260b4e16266bee45c8a8c938`, with the
bounded bulk-request intake correction in `4711983247ae912c48146465b02ffa9791e5ba7a`.
The 38 unit, 9 hosting and lifecycle tests, workspace Clippy and all six real-workerd contracts passed.
The actual cross-pack contract observes one Worker/DO service request containing three R2 GETs with exact
bytes; partial missing-object reads retain complete receipts. Both declared and chunked oversized JSON
requests are rejected before DO dispatch and a following metrics request completes normally.

### Workload clarification: interleaved large writes and historical reads

The user requested a realistic push/fetch sequence instead of treating one write phase and one read phase
as representative. The next experiment therefore has an explicit `interleaved-v1` identity: 16/64 MiB,
cold/warm, 32 pushes and 56 fetches. Every push publishes main plus an immutable historical ref, followed
immediately by a latest fetch. Every fourth cycle also fetches two older refs (with and without existing
objects), then latest again. Seven subsequent pushes replace 8 MiB of logical data with independent data.
The 64 MiB initial transfer is expected around 34 MiB; actual wire sizes must substantiate those labels.

The existing batch reports remain evidence for their original workload. R2's projected 9.004% saving is not
an interleaved-workload prediction, and its old batch matrix has not been run. The Rust runner and evaluator now implement this separate schema-v4 workload while retaining legacy behavior. Fresh matching interleaved baseline and
candidate traces will use the same runner and committed Worker, with scalar R1 versus batched R2 native
binaries, explicit hashes, unchanged budgets and the complete ordered fixture identity.

The interleaved implementation has passed independent source review, workspace tests and strict workspace
Clippy. Each trace must observe nonzero native compaction, and every large-push label is checked against
measured wire bytes. The final pre-measurement control uses index-only Git maintenance: multi-pack bitmap
and split commit-graph updates, with `pack.allowPackReuse=multi`, preserving the existing fsck/fsync settings.
Both native variants will use one frozen Git reference executable; its exact hash and the effective
receiver configuration are part of the new workload identity. Legacy batch maintenance stays unchanged.
This is an explicit practical Git configuration, not a claim that every Git deployment is slower.
