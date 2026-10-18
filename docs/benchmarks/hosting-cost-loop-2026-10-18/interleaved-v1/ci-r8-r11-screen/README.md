# R8–R11 isolated two-pair screens

[Run 39092442019](https://github.com/no-std-rs/git-zero/actions/runs/39092442019)
completed all four profiles and scoring at `7e7856a3f6c279a8f97956a4475cb12f847b1f7e`.
Each profile measured all four providers on the same free standard GitHub VM,
with fresh local workerd/R2/SQLite state, two pairs and one warmup per backend.
No paid Cloudflare deployment was used.

| Provider | Known fixed objective | Reduction from R8 |
| --- | ---: | ---: |
| R8: certified discovery | $2.05461643618 | — |
| R9: staged upload/publication | $1.99701543618 | 2.803492% |
| R10: historical prefix, inactive in this run | $1.99702143618 | 2.803200% |
| R11: peak in catalog and recovery | $1.94098593618 | 5.530497% |

This objective repeats each of four 32-push/56-fetch profiles 1,000 times,
plus one observed R2 peak held for 30 days. These are gross known subtotals
before allowances, subscription minimums and rounding. Worker CPU, DO duration,
monthly SQLite storage and external native compute remain unknown; the full
hosting cost is null. Elapsed time never substitutes for billed CPU.
`counter-scopes.json.gz` separates fixed operation receipts, whole non-stats
traffic including startup, and excluded observer work.

All 216 resource and 24 per-profile cost guards pass. Every comparison remains
**inconclusive**, with failed latency guards and no formal plateau eligibility:

| Comparison | Failed workload/profile latency groups |
| --- | ---: |
| R9 vs R8 | 3 |
| R10 vs R8 | 3 |
| R10 vs R9 | 6 |
| R11 vs R8 | 4 |
| R11 vs R9 | 7 |
| R11 vs R10 | 6 |

R11 versus R8 fails 16 MiB warm large pushes, 64 MiB cold historical fetches
without haves and large pushes, and 64 MiB warm latest fetches. The worst R11
sample is **136,237.074 ms** for 64 MiB warm `history_no_haves_12`, versus its
paired Git's 802.426 ms. All tails, individual ratios and complete guards are
retained. This screen supports no speed win or formal promotion.

R9 saves exactly 32 Worker and 32 DO callbacks per trial. R11 reduces timed SQL
writes from 99 to 86 at 16 MiB and from 103 to 88 at 64 MiB, including its actual
first-mutation migration. Across four profiles this removes 56 writes net.
R2 operations/bytes, storage and transfer sizes remain unchanged; SQLite stays
at 24,576 bytes. The fixture still contains seven later large pushes and random
historical fetches with and without older haves.

R10/R11 perform **zero historical projections** in both cold profiles, including
warmups. Real Git 2.53 echoes advertised `deepen-since` and `deepen-not`
capabilities, which R10's strict allowlist omitted. These samples retain the
ordinary full-read fallback; R10's measured cost is flat versus R9. Actual final
observer restore GETs/read bytes are zero too. No hypothetical selection saving
or restoration cost replaces those actual measurements. The independent
[diagnosis](history-selection-diagnosis.md),
[corrected real-client proof](../r10-history-candidate/git-capabilities/validation.md)
and [source review](../r10-history-candidate/git-capabilities/independent-review.md)
are separate from this failed activation evidence; the correction still needs
its own fresh screen.

Independent verification checked 16 reports, 264 internal checksums, complete
warmups and 32-push/56-fetch trials, fresh state, clean sources and pinned
source/binary/runtime/reference/VM identity. It retains 2,816 host/Git operation
ratios. Frozen Rust evaluator SHA256 `01b36df2f5f2019eeb09ebe79eb8ae80e9f86d0cdf39667c9fec9b0584e61fb8`
reproduces all four scores and six comparisons byte for byte. Intermediate
suites serve multiple comparisons within this declared batch; those comparisons
are correlated, not extra independent pairs.

The five original archives total 6,554,253 bytes and retain every raw report,
score and provenance file. Compressed tables keep all guards, receipts, native
metrics, migration and observer work. Extract profile archives under
`profiles/PROFILE/` to check `RAW_SHA256SUMS`; the original full evidence
inventory is `ORIGINAL_EVIDENCE_SHA256SUMS`. Verify this publication with
`sha256sum --check SHA256SUMS`.

After all guards and tails were reviewed, R11 is only the next provisional
known-cost comparison anchor. Latency has not passed. Corrected history,
packed catalog and any deferred update require reviewed source and a newly
pinned fresh screen. Final validation remains five fresh original-R1/final
pairs across all four profiles; no screen samples can be reused.
