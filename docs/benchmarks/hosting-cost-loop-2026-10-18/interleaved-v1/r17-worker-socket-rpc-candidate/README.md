# R17: bounded Worker service sockets

Source `c128197` and local correctness evidence passed independent review. The
first connection is paid; seven subsequent paired pushes each saved one Worker
request, with equal DO/R2/SQL work and identical public response bodies.

This is a correctness candidate. Total cost and speed remain unknown; no
performance screen, promotion or stopping decision is recorded here.

- [Evidence and limits](validation.md), [source review](source-review.md)
- [Machine-readable ledger](implementation.json), [packaging verification](verification.json)
- [Compact source and raw proofs](correctness-evidence.tar.gz)
- [Every capsule member](capsule-members.json.gz), [complete retained files](retained-artifacts.json.gz)
- [Exact source and binary maps](source-manifest.json.gz), [file hashes](SHA256SUMS)

Large payloads, generated Git/R2/SQLite state and ELF files are local hash-only;
their paths, sizes and SHA-256 values are included in the retained inventory.

The [independent ledger review](independent-ledger-review.md) approves exact docs commit `9135f96207e89faa5c566aaa2780991eb5644f82` and frozen production source `c128197cc2cf1761e93832c63eec4c25c1268249`. Fresh screens and final five-pair validation remain; this is no net-cost or speed result.
