# R5–R8 isolated two-pair screens

[Run 39087721879](https://github.com/no-std-rs/git-zero/actions/runs/39087721879)
completed all four profiles and scoring at `b2df9560c7461fee85dc6433c2fdf97437b196fb`.
Each profile used one standard public GitHub VM for all four providers, fresh
local workerd/R2/SQLite state per provider, two measured pairs and one warmup
per backend. No paid Cloudflare deployment was used.

| Provider | Known fixed objective | Reduction from R5 |
| --- | ---: | ---: |
| R5: 16 MiB chunks | $2.33324593618 | — |
| R6: fused retirement | $2.29544193618 | 1.620232% |
| R7: verified owned chunks | $2.29545493618 | 1.619675% |
| R8: certified native discovery | $2.05460443618 | 11.942226% |

The fixed objective repeats each of four 32-push/56-fetch profiles 1,000 times
and holds one observed R2 peak for 30 days. It includes large later pushes,
latest fetches and random historical refs with and without haves. These are
gross known subtotals before allowances, subscription minimums and rounding.
`counter-scopes.json.gz` separates fixed operation receipts from whole non-stats
traffic, including startup. Whole counters are never paired with fixed dollars.

All 216 resource guards and 24 profile-cost guards pass. Five comparisons pass
the separated 1% cost envelope. R7 versus R6 is cost neutral (tiny measured SQL
read variation), consistent with a CPU-only change whose compute cost is unknown.
R8 is 10.492495% below R7. All six comparisons remain **inconclusive** and are
ineligible for plateau evidence. Latency failures remain visible:

| Comparison | Failed latency groups |
| --- | ---: |
| R6 vs R5 | 8 |
| R7 vs R5 | 6 |
| R7 vs R6 | 4 |
| R8 vs R5 | 4 |
| R8 vs R6 | 5 |
| R8 vs R7 | 4 |

R8 versus R5 fails 16 MiB warm historical reads without haves and large pushes,
and 64 MiB cold large pushes and latest rechecks. Its worst retained individual
fetch is **116,947.318 ms**, at 64 MiB cold history-without-haves generation 20.
No speed win or formal acceptance is established. Worker CPU, DO duration,
monthly SQLite storage and external native compute remain unknown; full cost
is null. Local elapsed time is never treated as billed CPU.

Both cold profiles reduce native full reconstructions from 112 to 81 per trial,
with exactly 31 certified discovery responses. R7→R8 reduces 16 MiB cold GETs
685→494 and read bytes 2,638,998,010→1,882,586,450; at 64 MiB cold, GETs
907→654 and bytes 5,498,399,248→3,943,421,886. Warm reads remain zero. Storage,
SQLite size, pushed wire bytes and fetched pack bytes are unchanged. All four
profiles remain at 24,576 SQLite bytes.

Independent validation checked all 16 raw reports, 264 internal checksums,
32 complete trials and 2,816 operation ratios; source, executable, runtime,
reference and VM identities; fresh state and clean source. The frozen offline
Rust evaluator reproduced four scores and six comparisons byte for byte.
Intermediate suites are shared within the declared batch, so these comparisons
are correlated observations, not extra independent pairs.

The five original archives contain raw reports, all scores/comparisons and
source/runtime/binary provenance. Compressed tables retain all 144 latency
groups, guards, resource profiles, counters, discovery metrics and tails.
Extract profile archives into `profiles/PROFILE/` to check `RAW_SHA256SUMS`;
check this publication with `sha256sum --check SHA256SUMS`.

After reviewing every guard and tail, R8 is the provisional known-cost anchor.
That choice does not establish acceptance or stopping. R9/R10 and the separately
reviewed peak-catalog design still need their own fresh declared screen. Final
validation remains five fresh original-scalar-R1/final pairs across all profiles.
