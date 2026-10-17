# Local hosting cost optimization loop

This experiment optimizes known resource dollars using actual local workerd R2 and SQLite Durable Object
operations. It uses the Rust `hosting-bench` runner and offline Rust `hosting-cost` evaluator. Nothing is
deployed and no paid Cloudflare resources are used. See [the runtime instructions](../examples/repository-host/cloudflare/README.md)
and [the existing measurements](hosting-cloudflare-benchmarks.md) for topology and accounting details.

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

All results below are pending fresh measurements. Expected savings are hypotheses.

| State/design | Commit / pushed SHA | Paired samples | Actual counts and score | Decision/evidence |
| --- | --- | --- | --- | --- |
| Fresh baseline | Pending integration commit and push | Pending: 5 per profile | Pending | No fresh claim yet |
| Bundle pack and index in immutable chunks, preserving legacy reads | Pending implementation commit and push | Pending: 2-pair screen, then 5 per profile | Expected fewer Class A PUTs and cold GETs; bytes and budgets unchanged | Approved to implement; unmeasured |

For each measured row, retain baseline/candidate IDs, raw-report and score paths/hashes, per-profile actual
R2/Worker/DO/SQL counts, known-dollar reduction, storage and latency guard results, missing billing dimensions
and the reason for acceptance/rejection. Record reruns as additional evidence rather than replacing failures.
