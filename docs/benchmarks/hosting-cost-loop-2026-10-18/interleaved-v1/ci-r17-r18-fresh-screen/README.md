# Fresh R17–R18 two-pair screen

Run 39113545149, attempt 1, at `556fa3bcacb46bf7cf1f7a3c04c6b3db77c54667` completed all four profile jobs and scoring. All 8 reports and five original archives come from this fresh attempt. No earlier sample, report, score or comparison was reused.

The frozen Rust evaluator independently reproduced both scores and the comparison byte for byte. Known fixed objectives are:

- R17: $1.49964143618
- R18: $1.42325543618

R18’s measured known fixed subtotal is 5.093617591% below R17. Full hosting cost stays unknown. All 36 resource guards and 4 profile-cost guards were inspected: 0 resource failures and 0 profile-cost failures. Conservative separated >=1% known-cost envelopes: r18-vs-r17: pass. All 24 latency groups and 48 median/p95 limits were inspected; 5 groups fail 5 limits. Every failed-workload pair, all 1,408 individual ratios, 64 selected tails and every original raw tail remain retained. Largest measured host latency is 173.087 seconds against paired Git 0.952 seconds. No sample was discarded.

The following are actual fixed-window per-trial means across the two measured samples; all individual SQL-read variations stay in the original receipts. All actual HEAD counts are zero, so Class B equals GET attempts here.

| Profile | Provider | GETs | Read bytes | Worker requests | DO requests |
| --- | --- | ---: | ---: | ---: | ---: |
| 16mib-cold | r17 | 425 | 1,771,600,643 | 184 | 295 |
| 16mib-cold | r18 | 346 | 1,096,568,553 | 182 | 258 |
| 64mib-cold | r17 | 585 | 3,831,138,627 | 192 | 420 |
| 64mib-cold | r18 | 506 | 2,505,738,563 | 187 | 341 |

R18's one protected verified chunk changes actual GET counts, transferred bytes, ordinary DO calls and preparation consumption. Exact receipts, actual admission/hit/eviction events, selected-view gauges and every operation difference are retained. No parity or modeled savings is forced on these dimensions. Newly published chunks remain uncached and every cold latest-after-push fetch retains a positive actual GET. Both providers count each actual service Upgrade as one Worker request, including rotations, and each application RPC dispatch as one ordinary DO HTTP request. No DO-socket discount, 20:1 ratio, frame billing multiplier or elapsed-CPU estimate is used. The joint 32 MiB/256-entry gauges include at most one <=16 MiB protected payload plus complete retained ownership and the socket 262,144-byte reservation. Existing response-only gauges remain separate. Configured frame bounds are distinguished from actual counters. Startup and final-observer work stay physically visible and separate; source controls do not substitute for measured receipts or unrecorded key-level traces.

The comparison remains INCONCLUSIVE and plateau-ineligible. No formal acceptance, speed improvement, provisional selection or stopping condition is declared by this auditor. Five fresh original-R1/final pairs on every profile remain NOT DONE. Worker CPU, DO duration, monthly SQLite storage and Native compute remain unknown. Final observer restoration is physically retained and excluded from the fixed objective; observed epochs or zero HEAD counts do not prove permanent proof validity or free future GC. Final snapshots do not certify absence of later ACK, close or teardown traffic.

ARTIFACT_MAP.json records original paths and exact uncompressed/published SHA256 identities. JSON/TSV/source payloads use deterministic gzip of their exact bytes. SHA256SUMS covers every published payload. The five original archives appear once; extracted originals are omitted. Earlier completed and failed attempts remain separate and unchanged. R19 is not enabled by these source pins.

The [root review](root-guard-review.json) independently recomputes all 16 priced trials, 36 resource and four profile-cost guards, 24 latency groups and 48 limits, 1,408 ratios, 64 retained tails, and all 160 pairs across five failed latency groups. It reconciles startup/fixed scopes and the actual $0.076386 known saving: 158 R2 GETs ($0.05688), 116 DO calls ($0.0174), seven Worker requests ($0.0021), and six mean SQL read rows ($0.000006). Physical storage costs and writes are unchanged. After complete source, scope, guard and tail review, R18 is only the provisional known-cost anchor for the next fresh comparison. No formal promotion, speed win or stopping follows.
