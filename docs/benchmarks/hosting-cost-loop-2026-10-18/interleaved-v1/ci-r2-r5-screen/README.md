# R2–R5 isolated two-pair screens

[Run 39084173778](https://github.com/no-std-rs/git-zero/actions/runs/39084173778)
completed all four profile jobs and Rust scoring successfully. Each profile used
one standard public GitHub-hosted VM to measure R2, R3, R4 and R5 once each,
with two measured pairs and one warmup per backend. Services were local workerd,
R2 and SQLite emulation; no paid Cloudflare deployment was used.

| Provider | Known fixed objective | Reduction from preceding provider |
| --- | ---: | ---: |
| R2: bridge batches | $2.78119593618 | — |
| R3: separate payload/framing bounds | $2.70602893618 | 2.702686% |
| R4: generation witness | $2.49482893618 | 7.804795% |
| R5: 16 MiB cloud chunks | $2.33324193618 | 6.476877% |

R5 is **16.106524% below R2** in this fresh batch. The modeled objective repeats
each of four profiles 1,000 times and stores one observed R2 peak for 30 days.
Each trial has 32 interleaved pushes and 56 verified fetches, including seven
large later pushes and random older refs with and without haves. Fixed prices
use measured push/fetch rows; `counter-scopes.json` labels whole non-stats trace
traffic separately, including startup. These are gross known subtotals without
allowances, subscription minimums or invoice rounding.

All six comparisons pass every cost/resource guard and the separated 1% cost
envelope. All remain **inconclusive**: two-pair screens require final validation,
and latency guards fail. R5 versus R4 fails 16 MiB cold large-push, 16 MiB warm
history-with-haves, 64 MiB cold large-push and 64 MiB warm history-without-haves
guards. Its last group retains a 138,537.58 ms p95. No speed win, acceptance or
plateau is established. Worker CPU, DO duration, monthly SQLite storage and
native compute remain unknown; full hosting cost is null.

The intermediate suites are shared across comparisons within this prospectively
declared batch. They are correlated observations, not extra independent pairs.
Independent verification checked all 16 raw reports, 252 profile checksums,
run/attempt/source/binary/Worker/reference identities and each profile's shared
VM, fresh namespaces and clean sources. The frozen offline evaluator reproduced
all four CI scores and six comparisons exactly; raw reports remain unchanged.

`summary.json` is the compact result. Compressed guard/ratio tables retain all
216 resource checks and 144 latency groups. `hosting-scores.tar.gz` contains all
four scores, six comparisons and scoring provenance. Each profile archive has
raw reports, health/state/runtime/source/binary provenance and its own checksums.
Extract profiles into `profiles/PROFILE/`, then check `RAW_SHA256SUMS` to verify
the 16 original reports. Check this publication with `sha256sum --check SHA256SUMS`.

R5 is the next provisional known-cost anchor. This choice preserves all failed
latency evidence and does not count toward stopping. The final candidate still
needs five fresh pairs against the original scalar R1 across all four profiles.
