# Fresh R18–R19 two-pair screen

Run 39118509165, attempt 1, at `622b3482b8f460c2e9b24bedf1ccc5735634aab8` completed all four profile jobs and scoring. All 8 reports and five original archives come from this fresh attempt. No earlier sample, report, score or comparison was reused.

The frozen Rust evaluator independently reproduced both scores and the comparison byte for byte. Known fixed objectives are:

- R18: $1.42325343618
- R19: $1.29245743618

R19’s measured known fixed subtotal is 9.189930386% below R18. Full hosting cost stays unknown. All 36 resource guards and 4 profile-cost guards were inspected: 0 resource failures and 0 profile-cost failures. Conservative separated >=1% known-cost envelopes: r19-vs-r18: pass. All 24 latency groups and 48 median/p95 limits were inspected; 3 groups fail 3 limits. Every failed-workload pair, all 1,408 individual ratios, 64 selected tails and every original raw tail remain retained. Largest measured host latency is 167.269 seconds against paired Git 0.960 seconds. No sample was discarded.

The following are actual fixed-window per-trial means across the two measured samples; all individual SQL-read variations stay in the original receipts. All actual HEAD counts are zero, so Class B equals GET attempts here.

| Profile | Provider | GETs | Read bytes | Worker requests | DO requests |
| --- | --- | ---: | ---: | ---: | ---: |
| 16mib-cold | r18 | 346 | 1,096,568,553 | 182 | 258 |
| 16mib-cold | r19 | 153 | 569,108,040 | 180 | 229 |
| 64mib-cold | r18 | 506 | 2,505,738,563 | 187 | 341 |
| 64mib-cold | r19 | 354 | 2,449,962,286 | 186 | 332 |

R19's Multi production path can retain at most 32 verified chunks and considers every already-required verified miss in selection order. The 17 MiB secondary owning admission preference leaves 15 MiB unallocated; it is not owned memory, reserved capacity, a separate cache, or a 14 MiB response cap. Actual joint 32 MiB/256-entry gauges include pool backing/growth, complete payload ownership, keys, headers, response ring, actual/pending certificates/history/proofs and the 262,144-byte socket reservation. Individual responses up to 16 MiB stay eligible. Actual required pressure evicts newest secondary chunks then anchor before response LRU entries. All 704 measured R19 host rows and their gauges are reviewed, plus all warmups. Logical pruning, pressure/reset eviction and physical R2 retirement stay distinct. Pruning requires confirmed current Full authority or ordinary authoritative recovery; observers neither admit nor logically prune. Actual startup GETs remain paid and separate.

Actual GETs, read/written bytes, Worker/ordinary DO/SQL counts, selected-view gauges, response deltas, admission/hit/rejection/prune/pressure events and every operation difference are retained without forcing parity or a 338-GET source model. Per-key membership and internal attempt timelines are not exported by the frozen runner and are not invented. New published chunks still need positive naturally required cold GETs, while warm ordinary payload GET remains zero. Both providers count actual Upgrades as Worker requests and each application RPC as one ordinary DO HTTP dispatch. No 20:1 ratio, frame multiplier or elapsed-CPU estimate applies. Startup and final-observer full restoration, phase-shifting Upgrades/RPCs and observed epochs remain separate actual receipts; no permanent proof or future-free recovery credit is inferred.

The comparison remains INCONCLUSIVE and plateau-ineligible. No formal acceptance, speed improvement, provisional selection or stopping condition is declared by this auditor. Five fresh original-R1/final pairs on every profile remain NOT DONE. Worker CPU, DO duration, monthly SQLite storage and Native compute remain unknown. Final observer restoration is physically retained and excluded from the fixed objective; observed epochs or zero HEAD counts do not prove permanent proof validity or free future GC. Final snapshots do not certify absence of later ACK, close or teardown traffic.

ARTIFACT_MAP.json records original paths and exact uncompressed/published SHA256 identities. JSON/TSV/source payloads use deterministic gzip of their exact bytes. SHA256SUMS covers every published payload. The five original archives appear once; extracted originals are omitted. Earlier completed and failed attempts remain separate and unchanged. Separate joint-only admission and fresh-schema bootstrap proposals are not enabled by these source pins.
