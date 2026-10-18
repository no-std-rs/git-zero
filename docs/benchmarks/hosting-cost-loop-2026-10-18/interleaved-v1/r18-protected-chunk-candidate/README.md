# R18: one protected verified GET chunk

Source: `f1ba8f548507dca1e5ab78e5c5d20d8360ecee21`; parent approved R17 `c128197`.

One immutable chunk shares the existing 32 MiB/256-entry budget. Only ordinary verified GETs admit; pushes may retain immutable bytes and pressure can evict them.

The final local correctness pair records GETs **46→29**, identical Git responses and write/storage totals. Its 24.92/39.41 values are test seconds, not benchmark latency.

- [Implementation and evidence ledger](implementation-ledger.json)
- [Independent source-only review](source-review.md)
- [Compact raw evidence](raw-evidence.tar.gz), including exact source/binary maps, 38 fault rows, allocation/eviction proofs and full local artifact inventory
- [Checksums](SHA256SUMS) and [publication validation](publication-validation.json)

Large Git bodies, fixture/state payloads and ELF binaries remain local-only; their SHA256/size identities and precise exclusions are recorded inside the capsule. Historical overwritten binaries and unavailable runner versions are not represented as retained payloads.

Source approval precedes a separate ledger continuity review. No benchmark result, net-cost/speed win, promotion or stopping conclusion follows. The $0.05616 opportunity is conditional; full bill stays NULL.

Independent [ledger review](ledger-review.md) verifies all 4,006 local files and 2,007 capsule members. Its [compressed verification](ledger-review-verification.json.gz) and the [design review](independent-design-review.md) are added with exact identities in [root provenance](root-review-provenance.json). The seven-file author bundle is preserved at ledger commit `64c70cf`; current checksums include these additions. Large payloads remain local-only as documented.
