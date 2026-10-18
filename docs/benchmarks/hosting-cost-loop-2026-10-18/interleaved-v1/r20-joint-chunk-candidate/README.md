# R20: actual joint ownership for verified GET chunks

SOURCE `b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0`, exact parent R19 `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`, has independent SOURCE-only approval. This documentation commit awaits separate ledger review.

Generic `joint` becomes the default and omits only Multi's unallocated 15 MiB admission heuristic. The actual 32 MiB / 256 combined-entry / 32-chunk budget, 16 MiB chunk and response limits, pressure order, authority, pruning, publication and GC behavior remain. `off`, `on`/`one`, and old `multi` remain controls.

The local same-source 49-row comparisons record ordinary GETs **186→139 (16 MiB)** and **375→282 (clean 64 MiB)**. Write and storage guards match. Two canonical response pairs have exact decoded-object proofs instead of raw-byte equality: first16 has one −17 B variance; clean64 has one −45 B variance (plus two separately traced fallback variances). Actual wire bytes and representation differences remain explicit; no cause is claimed. These are correctness/accounting controls, not priced benchmarks or speed results.

- [Implementation/evidence ledger](implementation-ledger.json), [sealed source handoff](source-handoff.md), and [independent source review](source-review.md)
- [Independent conditional design review](independent-design-review.md)
- [Compact evidence capsule](raw-evidence.tar.gz), including `CAPSULE-MANIFEST.json` and exhaustive `LOCAL-ONLY-INVENTORY.json`
- [Publication validation](publication-validation.json), [owned cleanup](cleanup.json), and [checksums](SHA256SUMS)

All 18 stopped raw roots remain explicit: six roots from three completed pairs, five offline variance-proof attempts (one initial failed setup and four successful proofs), the initial failed64 runtime, and six final bounded controls. The source handoff is authoritative for the exact list: 7,684 regular files / 24,347,201,196 logical bytes. Large Git/R2/SQL/state, pack bodies, archives and ELF payloads remain local-only; all seven current mapped runtime paths and executable aliases are freshly verified. Historical v1 runner ELF is absent and is not presented as a current file.

Limits remain explicit: first16 final observer traffic is UNKNOWN/NULL after its last snapshot; failed64's actual 503 body was not saved and EAGAIN cause is unknown; traced64 is fallback-only because inherited Trace2 disabled lineage; top-key display is not a whole-DO timeline; fault SQLR/SQLW stay NULL/incomplete. GC LIST-before-next-PUT is proven in 11/13 following pushes, with two source exclusions. The unfired partial-delete attempt remains paid and separate. HTTP's late empty process snapshot is disclosed; no rerun concealed it.

Raw `/dev/shm` state is volatile local-only and visible in the escalated runtime namespace; no power-loss or timing claim. Actual volatile artifacts total 18,411,096,507 B below the authorized 24 GiB planning limit; every runtime scope passed the 40 GiB MemAvailable threshold. Warm/HTTP per-scope estimates were low by about 9.2 MB each; total budget stayed within its limit. No source, evidence or compiler cache was deleted.

The conditional 193-GET / $0.06948 model is not a measured target; unconditional net lower bound is zero and the full bill remains NULL. Fresh independent measurement, all-profile guards and the mandatory final five original/final pairs remain separate. No acceptance, plateau or stopping claim follows.
