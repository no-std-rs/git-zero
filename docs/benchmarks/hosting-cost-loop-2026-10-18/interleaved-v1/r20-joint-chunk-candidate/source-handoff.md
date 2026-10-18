# R20 Joint — exact SOURCE handoff

Ready for independent SOURCE-only review: `b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0`, exact parent `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`. Worktree `/tmp/git-zero-hosting-joint-chunk` is clean. Native tree `ff99eaf58a5e4f68e744d0b50ddee2af67aff1cf`; Worker production tree stays `7f41db4407963ed04e6cbe19556b1416633caf18`.

Only production policy change: new generic `joint` omits the old Multi nonempty-pool17MiB heuristic and becomes default. `off`, `on`/`one`, and old `multi` retain their meanings. Actual owning32MiB,256 combined entries,32 chunks,16MiB chunk/response eligibility and262144-byte socket reservation remain. Required pressure order, first-fit/no owner eviction for admission, currentFull-confirmed pruning, observer/startup/PUT no-admission, authority, Full tokens, Worker opacity, NULL accounting and synchronous publication/GC are unchanged. No productionWorker, dependency, scorer, workload, rates or schema changes.

Final checks:130 library tests passed in a clean Git environment; all-target Clippy `-D warnings`, formatting and diff checks passed. Actual Native `2633e97fa9f50afa3b17fe53bdc39e42b1f0ff011f3bed4084c717afc4ffc507` and v2 runner `29fa1c4cfb9a9175b505fb3269ea0cdc05e16f6a2c78981f0caeebcc5287b3e3` remain byte-identical across canonical pairs and all bounded gates. All before/after maps freshly verified against current actual files. Versioned binaries and both driver sources remain in `/tmp`; historical v1 runner ELF is absent and is represented only by its old identity records.

| Closed scope | Multi → Joint actual result | Limits |
|---|---|---|
| First16,49 ordinary rows | GET186→139; read951,990,549→664,299,676B |39 raw-equal +one decoded-equal −17B representation. Final manifest observer traffic after last snapshot UNKNOWN/NULL; no full-run total claim. |
| Clean64,49 ordinary rows | GET375→282; read3,418,793,827→2,987,738,608B |Startup12GET/80,369,928B each, admission0.39 raw-equal +one decoded-equal −45B recheck24 representation. |
| Clean64 after final observer | W194→192;DO392→365;SQLR293→245;SQLW14 equal |Final observer actual +2W/+2DO/+6SQLR each,0GET/readB. Each39/39 reads and7/7 receives consumed; lineage400B and actual live Native Git env known-only. |
| All ordinary pair write guards | Equal PUT/DELETE/LIST/CAS, written/live/peak objects/bytes, SQLW/SQLite/manifest |Clean64 written46,541,518B; live=peak126,897,694B;15 objects;SQLite24576B. No cost/speed/acceptance claim. |

Protected pressure evictions are a measured tradeoff: clean64 Multi0/Joint23; no resident-superset or model-count claim. Exact packet controls, object ordering/delta differences, decoded OID/type/size/content SHA256, strict indexpack/fsck and wanted-source archive proofs retain actual wire differences; their timing/nondeterminism cause is not asserted.

All six exact-commit Joint controls passed and stopped: `joint-allocation-01`, `joint-plan-01`, `joint-faults-01`, `joint-restart-01`, `joint-warm-01`, `joint-http-01` under `/dev/shm/git-zero-joint-correctness-20261018`. Compact mirrors and source maps are in `/tmp/git-zero-joint-correctness-manifests/<scope>`.

- Allocation: actual initial retained heap delta30,061,177B ≤ conservative charge30,305,781B. Twelve productive responses16,652,187B each (>14MiB and≤16MiB), paid GET refills4 then13×11, required response retention reclaims pool, restartadmit0. Request-local heap peaks are separate from retained ownership.
- Held signed plan: real cache pressure; charged discard+1, evictions+15, admissions+8; valid productive fallback. Wrapper trace is a separate control, not frozen production-pair traffic.
- Faults:38 complete records,10 read modes,14 publication attempts,13 same-Native distinct next pushes. GC LIST-before-next-PUT established in11/13; `unknown-sql` and `gateway-lost-reply` are explicit source exclusions. Unfired `partial-delete` attempt0 stays separate (+1PUT1295B/+6GET83,887,373B), fired attempt1 retained. Final SQLR/SQLW NULL and accounting_complete=false: no complete bill claim.
- Restart: zero admission/owners on same-root reopen, subsequent actual prepared receive/latest/history pass. Raw cumulative startup counters are paid, not free.
- Warm: zero chunk admission and no GET/readB increase after paid startup through real pushes/latest/history.
- HTTP: preparation/socket disabled, consumed read/receive0, real Joint admissions/hits and valid pushes/latest/history. Its process snapshot was late and empty after the run stopped; no live HTTP proc-env claim. Before/after all7 runtime/source maps and launch/context/lineage evidence remain. No replacement run conceals this gap.

Private Native unit controls cover actual32-owner/256-entry boundaries, continued first-fit scan, metadata charge and eviction order for both modes; the production allocator complements those synthetic boundaries. No runtime upload-advertisement request/certification coverage is claimed for these pair workloads.

Preserved separate scopes: failed64 Native metrics503 with EAGAIN after public200; root cause UNKNOWN and actual503 body unavailable from old helper, never replaced by inferred text, no successful after-map. Traced64 has lineage0/read39unsupported and receive7/7: fallback-only coverage, not canonical eligible workload counts. Both decoded fallback variances (+39B/+22B), all old failed/offline attempts and histograms remain. Displayed per-key histograms have no epoch token and may disagree with aggregate even when displayed==tracked; closure is UNKNOWN/NULL when unproved. Aggregate actual receipts remain authoritative.

Full inventories cover18 raw roots,7,684 regular files and24,347,201,196 logicalB. `/tmp` completed evidence remains unchanged; raw `/dev/shm` artifacts are volatile local-only and must be inspected in the escalated real host namespace (sandbox hides mount). No power-loss durability or performance claim. Final volatile use18,411,096,507B stays below authorized24GiB; each new scope passed≥40GiB MemAvailable. Warm/HTTP per-scope estimates were low by~9.2MB each; actual total stayed within budget. Source/targets/binaries/compact evidence stay in `/tmp`; no evidence, source or compiler cache was deleted.

Final owned cleanup:0 matching Native/runner/Worker processes;8845 and8846 both bind available. Full receipt `/tmp/git-zero-joint-final-owned-cleanup.json`; final MemAvailable53,223,489,536B and/tmp5,559,894,016B free.

Machine-readable handoff: `/tmp/git-zero-joint-chunk-source-handoff.json` (all exact maps, logs, contracts, inventories, disclosure and cleanup/resource receipts). This is SOURCE-only readiness. No source/ledger screen, CI, push, paid deploy, primary integration, cost/speed acceptance or stopping claim. Docs-only ledger/capsule follow after source approval.
