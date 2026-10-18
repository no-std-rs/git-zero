# Fresh R19/R20 two-pair screen independent review

Run [39128927970](https://github.com/no-std-rs/git-zero/actions/runs/39128927970), attempt1, head `d5cdefe53fc24f34114292b7d19563e73afea293`. All four profiles and scoring completed. Eight original schema4 reports, sixteen measured cost trials, all warmups, and five original artifacts are preserved. The frozen Rust evaluator reproduced both schema2 scores and the comparison byte for byte.

Known fixed objective USD (1000 repeats EACH profile, actual R2 peak once30d; frozen2026-10-17 rates):

- R19: `$1.292455436180`; conservative envelope `[1.29245143618, 1.29245943618]`
- R20: `$1.212471436180`; conservative envelope `[1.21247143618, 1.21247143618]`

All36 resource guards,4 profile-cost guards,24 latency groups and48 median/p95 limits were inspected. Failed latency groups: 5; failed limits: 5. Cost-envelope and all guard outcomes are retained in `guard-report.json` and `comparison-summary.tsv`, including every failure.

- 16mib-warm / history_no_haves: median pass, p95 FAIL.
- 16mib-warm / latest_recheck: median pass, p95 FAIL.
- 64mib-cold / history_with_haves: median pass, p95 FAIL.
- 64mib-warm / history_no_haves: median pass, p95 FAIL.
- 64mib-warm / latest_fetch: median pass, p95 FAIL.

All1408 individual host/Git ratios, all48 provider/profile group maxima,64 independently reconstructed top8 tails and every pair in every failed group are retained. Largest host operation: 182.773174s, paired Git 1.096087s. Largest individual ratio: 224.288607130. No operation, sample, severe tail or unsupported preparation row was discarded.

Actual per-trial mean fixed-window counters across two measured samples (all individual values and SQL variation retained):

| Profile | Provider | Class B | R2 read bytes | Worker requests | DO requests | SQL rows read |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 16mib-cold | r19 | 153.0 | 569108040.0 | 180.0 | 229.0 | 262.0 |
| 16mib-cold | r20 | 85.0 | 219786498.0 | 179.0 | 214.0 | 260.0 |
| 16mib-warm | r19 | 0.0 | 0.0 | 178.0 | 208.0 | 262.0 |
| 16mib-warm | r20 | 0.0 | 0.0 | 178.0 | 208.0 | 260.0 |
| 64mib-cold | r19 | 354.0 | 2449962286.0 | 186.0 | 332.0 | 265.0 |
| 64mib-cold | r20 | 229.0 | 1849065488.0 | 183.0 | 285.0 | 265.0 |
| 64mib-warm | r19 | 0.0 | 0.0 | 179.0 | 210.0 | 265.0 |
| 64mib-warm | r20 | 0.0 | 0.0 | 179.0 | 210.0 | 265.0 |

Every API classification, byte total, phase/category closure, counter epoch and complete receipt was checked. There are 0 service closure violations, 0 retained transport diagnostic failures and 0 retained protected-cache diagnostic failures. Actual write/space/data comparisons with differences: 0; all exact fields and differences remain in `actual-data-write-space-equivalence-review.json`. Worker categories may total fewer requests than ordinary DO dispatches because socket RPCs are counted at their actual service boundaries. No frame multiplier is charged.

R19 source `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`, NativeSRC `521d427a056711a58b757c9e52f3d5e047937f92`. R20 source `b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0`, NativeSRC `ff99eaf58a5e4f68e744d0b50ddee2af67aff1cf`. Common Worker tracked six-module tree `7f41db4407963ed04e6cbe19556b1416633caf18`. Complete source, current-run binary/runtime, fresh-state, chronology and ancestry receipts are retained; no historical ELF identity is imposed. Primary differs from pinned R20 only by the reviewed common error-only runner files.

R20 Joint removes only Multi's unallocated15MiB secondary preference. Both retain actual32MiB/256 joint budget, pool32,16MiB chunks/responses,262144 socket reservation, first-fit verified ordinary GET admission with no live-owner replacement, required pressure newest secondary then anchor then response LRU, and pruning only after confirmed current Full authority. Startup/recovery/observer/PUT/warm paths do not admit. Newly published chunks still require naturally paid cold GETs. Runtime cache hits/bytes/owners/entries/growth/prune/reject/pressure/reset counters and socket16RPC rotation/ACK receipts are preserved. R20 is not a resident superset. Source ownership gauges are not RSS or transient heap; bounded archived topkeys are not a complete ordered per-key timeline.

Startup, ordinary fixed windows, whole nonobserver trace, observer restoration and unclassified phase counts remain separate actual receipts. Whole-minus-observer equals fixed-plus-actual-startup only where verified; no startup count is prescribed. Worker CPU, DO duration, monthly SQLite storage and Native compute remain unknown; full bill is NULL. Final snapshots do not establish permanent proof validity, free future GC/recovery, or absence of later ACK/close/teardown traffic. Ready push latency includes reference maintenance; elapsed is never billed CPU.

The screen is formally INCONCLUSIVE and plateau-ineligible. No speed, acceptance, provisional selection or stopping claim is made. Root must independently review all sources/scopes/guards/tails before any selection or unfreeze. Mandatory FIVE fresh original-R1/final pairs on EACH of all four profiles remain NOT DONE.

Publication contains five exact original embedded tar.gz payloads and deterministic gzip of exact derived metadata/scripts. Original API ZIP wrappers stay local-only with complete SHA256/member mappings in `transport-archive-index.json` and `all-four-outcomes.json`; no original is removed. Large fixtures, ELFs and runtime R2/SQLite state were not in these bounded CI artifacts and are not uploaded. Prior samples/capsules/reviews remain separate and unchanged. All shadow/API work remains cancelled. This is local staging only; nothing published or pushed by auditor.
