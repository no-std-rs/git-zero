# R11–R13 screen analysis

This fresh two-pair screen uses 16/64 MiB × cold/warm Native views, 32 pushes and
56 verified nonempty fetches per trial, an excluded warmup, new service state
and a shared standard GitHub VM for compared providers within each profile.
Actual workerd/R2/SQLite run locally on those VMs. No paid Cloudflare deployment
or standalone Worker Git codec was used; the external Rust/Git codec remains
part of the topology. Native and Git order alternates AB/BA within paired trials.

The fixed objective repeats each profile's exact measured operation mix 1,000
times and holds its observed R2 peak once for 30 days. Gross dated request/row
charges exclude allowances, subscription minimums and rounding. Worker CPU,
DO duration, monthly SQLite retention and external Native compute remain
unknown; the complete hosting bill is null. Client elapsed time is not CPU.
Whole non-stats startup traffic and observer work have separate actual receipts.

## Decisions and complete guards

| Comparison | Known-cost reduction | Failed latency groups |
| --- | ---: | ---: |
| R11.1 / R11 | 2.906940% | 5 |
| R12 / R11 | 5.581290% | 5 |
| R12 / R11.1 | 2.754420% | 5 |
| R13 / R11 | 7.025203% | 4 |
| R13 / R11.1 | 4.241563% | 4 |
| R13 / R12 | 1.529266% | 6 |

All six comparisons pass separated conservative ≥1% cost envelopes, all 216
resource checks and all 24 per-profile cost guards. Every comparison remains
formally inconclusive and ineligible for plateau evidence. The coordinator
independently recalculated all 144 workload/profile latency groups and 288
median/p95 limits, recomputed all 2,816 individual host/Git ratios and checked
all 128 retained tails. The independent measurer separately rebuilt every
resource and latency distribution from actual raw operations.

R13 versus R11 passes all medians but fails p95 for 16 MiB cold historical
fetches with haves, 16 MiB warm historical fetches with haves and latest fetches,
and 64 MiB warm large pushes. Its worst measured operation is 57,956.071 ms for
16 MiB warm history_with_haves_16, versus paired Git's 382.851 ms. The cold
history_with_haves_08 sample takes 19,247.362 ms versus Git's 115.241 ms.
All slow samples and all 29 failed groups remain in the evidence. Their causes
are not established by service-header timing. No speed win or all-guard
promotion is claimed. R13 is only a provisional known-cost comparison anchor;
final validation still requires five fresh original-R1/final pairs on all four
profiles, with no reused screen samples.

## Actual activation and storage

Every corrected cold trial, including warmups, observes 16 timed historical
projections and seven timed full restores; original R11 observes zero of each.
The final timed selected view has 3 of 11 packs. Corrected 16 MiB cold changes
494 GETs / 1,882,586,450 read bytes / 376 Worker+DO requests into 425 GETs /
1,771,600,643 bytes / 367 requests. Corrected 64 MiB cold changes 654 GETs /
3,943,421,886 bytes / 498 requests into 585 GETs / 3,831,138,627 bytes / 492
requests. These are actual receipts, not substituted prefix projections.

Each corrected final cold observer performs a real full restore: 11 GETs and
39,265,016 bytes at 16 MiB, or 13 GETs and 65,116,174 bytes at 64 MiB. Original
R11's final observer restoration reads zero object bytes. Observer restoration
is retained in complete physical scope and excluded from the fixed objective.
All actual HEAD counts are zero. Both warm profiles have zero GETs and HEADs.

| Profile size | R11 / R11.1 SQL writes | R12 writes | R13 writes |
| --- | ---: | ---: | ---: |
| 16 MiB, either cache mode | 86 | 73 | 66 |
| 64 MiB, either cache mode | 88 | 75 | 68 |

Across the eight measured trials per provider, SQL read totals are
2,329 / 2,343 / 2,526 / 2,474 and write totals 696 / 696 / 592 / 536 in provider
order. Every provider performs 264 Class A operations, 168 deletes and writes
425,516,692 actual R2 bytes. Timed GET totals fall from 2,296 to 2,020.
First eligible schema work is inside the first timed push: 16 MiB R11/R11.1
10 reads/3 writes versus R12/R13 18 reads/4 writes; 64 MiB 10/5 versus 20/6.
Warmup and sample receipts agree. R13 adds no schema column beyond R12.

Final SQLite stays 24,576 bytes; initial push SQLite is 20,480 bytes for every
provider. Peak R2 storage stays 39,265,016 bytes at 16 MiB and 65,116,190 bytes
at 64 MiB. All read/write-byte, transfer, retained-pack, storage and joint cache
guards pass. Raw DO-instance epoch changes are retained; the trace cannot
attribute every change to a specific proof or GC path. No observer certifies or
repairs pending production state.

## Provenance and reproduction

The run is attempt 1 at `5c616c23469fff9c7f64789dc29dc640bbc0dfc1`. Whole Worker
source tree is `22902cf4688aa7ee5da3705adceaaee0a2f08caa`; index SHA256 is
`dfa5d305b6dc511cdb85eb8b7ee717d09f70e7a0435160f29cecf75126c865c1`.
All three source modules, actual Native provider trees, scalar reference,
common runner/configuration and shared per-profile VM identity were verified.

Fresh common hosting-cost SHA256 is
`c28841b7ec20ccf6170c4c246090da31e251a8119189d4354df6c9fcf9c497e3`.
Linked Native/library changes alter common executables despite unchanged
runner/scorer/pricing source, manifests and lockfile. Fresh common hashes agree
across all four profiles, and the scalar reference remains unchanged. Frozen Rust evaluator SHA256
`01b36df2f5f2019eeb09ebe79eb8ae80e9f86d0cdf39667c9fec9b0584e61fb8`
reproduces every one of the four CI scores and six comparisons byte for byte.
No earlier report or score supplies a new sample. The six comparisons share
fresh intermediate suites and are correlated, rather than additional pairs.

All 16 reports, 268 original internal hashes and 379 evidence-file hashes were
independently verified. The five original archives total 6,612,858 bytes, each
below 4 MiB and together below 20 MiB. The separate 1,498,303-byte independent
bundle has SHA256
`345e2f84dd79d32de69d4a91140da3fcd074f6d21901fc2ce0199e4fcaa00fb4`;
all 94 member hashes match. `publication-map.json` maps generated tables to
unchanged originals or deterministic gzip. Original inventories retain their
original paths; extract CI profile archives under `profiles/PROFILE/` and the
independent bundle at a separate temporary root for the corresponding checks.
`root-guard-review.json.gz` contains the coordinator's complete recalculation.
