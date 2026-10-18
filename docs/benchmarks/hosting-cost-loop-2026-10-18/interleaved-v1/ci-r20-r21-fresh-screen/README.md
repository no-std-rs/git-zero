# Fresh R20/R21 two-pair screen independent review

Run [39145424189](https://github.com/no-std-rs/git-zero/actions/runs/39145424189), attempt1, head `f76e32b1c4e4fd51119c2f452209162fde812b32`. All four profiles and scoring completed. Eight original schema4 reports, sixteen measured cost trials, all warmups, and five original artifacts are preserved. The frozen Rust evaluator reproduced both schema2 scores and the comparison byte for byte.

Known fixed objective USD (1000 repeats EACH profile, actual R2 peak once30d; frozen2026-10-17 rates):

- R20: `$1.212477436180`; conservative envelope `[1.21247143618, 1.2124834361799999]`
- R21: `$1.185473436180`; conservative envelope `[1.18547143618, 1.18547543618]`

All36 resource guards,4 profile-cost guards,24 latency groups and48 median/p95 limits were inspected. Failed latency groups: 5; failed limits: 5. Cost-envelope and all guard outcomes are retained in `guard-report.json` and `comparison-summary.tsv`, including every failure.

- 16mib-cold / history_no_haves: median pass, p95 FAIL.
- 16mib-warm / history_no_haves: median FAIL, p95 pass.
- 16mib-warm / large_push: median FAIL, p95 pass.
- 16mib-warm / latest_fetch: median pass, p95 FAIL.
- 16mib-warm / latest_recheck: median pass, p95 FAIL.

All1408 individual host/Git ratios, all48 provider/profile group maxima,64 independently reconstructed top8 tails and every pair in every failed group are retained. Largest host operation: r20/64mib-cold/history_no_haves_24, sample1: 180.645887s, paired Git 0.977342s. Largest individual ratio: 199.593578984, r21/16mib-warm/history_no_haves_20, sample0. No operation, sample, severe tail or unsupported preparation row was discarded. Two reviewer-only initial predicate failures are retained exactly: npm extraneous notices were initially overconstrained, and f64 numeric event deltas were initially required to be Python ints. Corrected checks preserve actual notices and exact whole numeric deltas; they change no raw bytes, score, guard or sample.

Actual per-trial mean fixed-window counters across two measured samples (all individual values and SQL variation retained):

| Profile | Provider | Class B | R2 read bytes | Worker requests | DO requests | SQL rows read |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 16mib-cold | r20 | 85.0 | 219786498.0 | 179.0 | 214.0 | 264.0 |
| 16mib-cold | r21 | 53.0 | 131883773.0 | 179.0 | 210.0 | 260.0 |
| 16mib-warm | r20 | 0.0 | 0.0 | 178.0 | 208.0 | 262.0 |
| 16mib-warm | r21 | 0.0 | 0.0 | 178.0 | 208.0 | 262.0 |
| 64mib-cold | r20 | 229.0 | 1849065488.0 | 183.0 | 285.0 | 265.0 |
| 64mib-cold | r21 | 191.0 | 1734213513.0 | 183.0 | 277.0 | 265.0 |
| 64mib-warm | r20 | 0.0 | 0.0 | 179.0 | 210.0 | 265.0 |
| 64mib-warm | r21 | 0.0 | 0.0 | 179.0 | 210.0 | 265.0 |

Every API classification, byte total, phase/category closure, counter epoch and complete receipt was checked. There are 0 service closure violations, 0 retained transport diagnostic failures and 0 retained protected-cache diagnostic failures. Actual write/space/data comparisons with differences: 0; all exact fields and differences remain in `actual-data-write-space-equivalence-review.json`. Worker categories may total fewer requests than ordinary DO dispatches because socket RPCs are counted at their actual service boundaries. No frame multiplier is charged.

R20 source `b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0`, NativeSRC `ff99eaf58a5e4f68e744d0b50ddee2af67aff1cf`. R21 source `95fdf58f6bd7298736cfe124725e4f7e6c9aceb9`, NativeSRC `4717edd249bcb85edec47674787aa1ecf9f7d8f1`. Common Worker tracked six-module tree `7f41db4407963ed04e6cbe19556b1416633caf18`. Complete source, current-run binary/runtime, fresh-state, chronology and ancestry receipts are retained; no historical ELF identity is imposed. Primary differs from pinned R21 only by the reviewed common error-only runner files.

Both use Joint, ordinary verified GET-only first-fit immutable admission and the actual32MiB/256 joint budget, pool32,16MiB response/chunk limit and262144 socket reservation. R20 keeps legacy optional upload-response eviction. R21 defaults to Fit only for Cloudflare+cold+Joint+nonempty protected ownership: checked preflight and exact cloned owning-weight/metadata recheck include full-new-ring growth on top of old backing; optional pressure/allocation failure skips without evicting or reordering existing owners. Successful commit adds no further ring allocation. Discovery/mandatory metadata pressure retains newest-secondary, anchor, response-LRU eviction; logical prune still needs confirmed current Full authority. Neither provider uses the historical Multi preference. Startup/recovery/observer/PUT/warm paths do not admit immutable chunks. Newly published chunks retain actual cold GETs. All physical cache/retention/pressure/reject/prune/reset and socket16RPC/ACK diagnostics remain in the review. Same-operation Fit skips do not imply zero mandatory pressure. Owning gauges are not total request heap, RSS or allocator-internal overlap; bounded topkeys are not a full ordered per-key timeline. Header clone remains infallible as before. No resident-superset or causal saving is inferred.

Startup, ordinary fixed windows, whole nonobserver trace, observer restoration and unclassified phase counts remain separate actual receipts. All actual R2 GET/HEAD/PUT/LIST/delete and SDK ordinary request attempts, SQL rows and byte counts are charged at the frozen rates; absent/lost accounting would invalidate the estimate, never become free usage. Whole-minus-observer equals fixed-plus-actual-startup only where verified; no startup count is prescribed. Worker CPU, DO duration, monthly SQLite storage and Native compute remain unknown; full bill is NULL. Final snapshots do not establish permanent proof validity, free future GC/recovery, or absence of later ACK/close/teardown traffic. Ready push latency includes reference maintenance; elapsed is never billed CPU.

The screen is formally INCONCLUSIVE and plateau-ineligible. No speed, acceptance, provisional selection or stopping claim is made. Root must independently review all sources/scopes/guards/tails before any selection or unfreeze. Mandatory FIVE fresh original-R1/final pairs on EACH of all four profiles remain NOT DONE.

Publication contains five exact original embedded tar.gz payloads, deterministic gzip of exact derived metadata/scripts and a portable offline reconstruct-publication.py. Its published mappings verify and reconstruct all exact payloads and all original internal checksums into a fresh directory. Original API ZIP wrappers stay local-only with complete SHA256/member mappings in `transport-archive-index.json` and `all-four-outcomes.json`; no original is removed. Large fixtures, ELFs and runtime R2/SQLite state were not in these bounded CI artifacts and are not uploaded. Prior samples/capsules/reviews remain separate and unchanged. All shadow/API work remains cancelled. This is local staging only; nothing published or pushed by auditor.
