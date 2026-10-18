# R19: bounded verified GET chunk cache

Source `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`, parent approved R18 `f1ba8f5`.

Up to32 immutable chunks share the existing32MiB/256-entry budget. Ordinary verified GETs admit without replacing live residents; confirmed Full authority prunes, and real memory pressure can evict.

Local ONE/MULTI correctness pairs record GETs **131→47 (16MiB)** and **223→119 (64MiB)** with62 identical response pairs and matching mutation/storage totals. These are not priced benchmarks or speed results.

- [Implementation/evidence ledger](implementation-ledger.json)
- [Independent source review](source-review.md) and [design review](independent-design-review.md)
- [Compact raw evidence](raw-evidence.tar.gz), including full local SHA/size inventory
- [Publication validation](publication-validation.json) and [checksums](SHA256SUMS)

All15 attempt scopes remain explicit: ten accepted, one earlier corrected R19 source policy, and four failed drivers. Large Git/state/ELF payloads remain local-only; identities are inventoried, not claimed published.

The17MiB later chunk-admission ceiling is an unallocated heuristic, not retained memory or a14MiB response cap. Full bill remains NULL; the$0.12168 model stays conditional. Ledger review, fresh measurement and final five original/final pairs remain separate.

The original d565 ledger omitted seven actual runtime paths from its inventory. This correction adds their verified identities; the blocked original bundle/review remain preserved. Runtime bytes stay local-only.

[Corrected ledger review](ledger-review.md) and [verification](ledger-verification.json.gz)
independently verify all 5,135 local files and 2,241 capsule members. The
[source and ledger identities](root-review-publication.json) distinguish approved
continuity from the separate fresh cost screen.
