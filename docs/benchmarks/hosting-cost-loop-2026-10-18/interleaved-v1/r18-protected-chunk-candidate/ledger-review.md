# R18 independent docs-only ledger continuity review

**APPROVED LEDGER CONTINUITY ONLY:** clean commit `64c70cf64e2815a4dc2b52d317694158490ba0eb`, exact parent approved source `f1ba8f548507dca1e5ab78e5c5d20d8360ecee21`. No blocking finding remains. This approval establishes faithful publication and retained local evidence identity; it does not establish a performance, net-cost, resource-guard, promotion, deployment, acceptance or stopping result.

I independently read the seven published documents, both handoffs, the capsule indexes and original review artifacts. I verified every capsule member and streamed every complete local inventory file, without reusing the author's or my earlier file hashes. Work was read-only except this report, its verification JSON and its reproduction script under `/tmp`. No candidate source/docs edits, runtime, build, test, benchmark, delegate, push, CI, deployment or root-screen operation occurred. R19's worktree and ports were not touched.

## Exact commit and public files

The clean branch remains `perf/hosting-protected-chunk` in `/tmp/git-zero-hosting-protected-chunk`. The source commit's parent remains approved R17 `c128197cc2cf1761e93832c63eec4c25c1268249`. The ledger adds exactly seven files under `docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r18-protected-chunk-candidate`; no path outside that folder differs from its source parent. Native source tree remains `c5b06db80540cc21a80e1ad19b4b8d1086425749`, and Worker source tree remains `7f41db4407963ed04e6cbe19556b1416633caf18`. `git diff --check` passes.

All seven public identities and all six SHA256SUMS entries were independently verified. Their total is **1,402,881 bytes**.

| Public file | Bytes | SHA256 |
| --- | ---: | --- |
| README.md | 1,304 | `5104806ec7c5d6f70c33dd241e4633ada0cd78530ad8531f800f697b01adcb97` |
| SHA256SUMS | 511 | `57a66806380ce6ad7c89f4dc707f84bfcf5426835d98cf9274e5bf2d2511fd86` |
| cleanup.json | 408 | `b7c53ea4ac95de334313102ad31e9cf3f66e52d5ece8a18676326545e481a932` |
| implementation-ledger.json | 27,338 | `3c316cecd221c836f8537c1e69a3cf31e30b4495af59151de0805acc5f01a376` |
| publication-validation.json | 923 | `cee8a981139bf971390777c96b45ec24ba7a9ee161d4d107a6662e1facb4724b` |
| raw-evidence.tar.gz | 1,361,154 | `e2b4048adf537246b6fe2be4e5b4c109dd0afbe7d3ed2fedeb7ce572aad8083f` |
| source-review.md | 11,243 | `7876f953552db52e91818ae179cc03b919cccf43f0a293a87bbf5a23ad2824b1` |

## Capsule and full local inventory

The gzip is one complete CRC-checked stream with normalized zero timestamp and no extra stream/trailing data. Its tar contains **2,007 unique safe relative regular-file members**, all mode 0644, uid/gid zero, empty owner names, timestamp zero, no links, PAX records, absolute paths, parent traversal or hidden extra headers. Tar padding is zero. Uncompressed tar size is **16,762,880 bytes**; member payloads total **15,052,156 bytes**. No extraction into a filesystem was performed.

The 705,702-byte `ledger/capsule-members.json` has SHA256 `7b93dd23ec9be8f092b5375010448e802a39c03141bef88d8aabb8c4aa6dbfd4`. Its **2,006** records cover every other member exactly once; its self-exclusion is explicit. I compared all **2,005 named local-source payloads** byte-for-byte against their local raw/source/review files. All 64 approved-source members also match `git show` at the exact approved source commit. The remaining generated member is the full local inventory, validated by its externally pinned identity and every underlying file.

That full inventory is **2,112,107 bytes**, SHA256 `d1bf6fc23f7a80d9a1fb78740e25ffc3b619a98806a07780065d448490e52b9f`. I independently streamed **all 4,006 files / 3,953,598,095 logical bytes** and matched every SHA256, size and recorded frozen stat, checking stat identity before and after reading. No earlier hashes were reused in this review. The author's separate methods—2,129 earlier independent hashes reused after frozen size/mtime checks and 1,877 newly streamed files—remain honestly recorded; they are not substituted for this verification.

The full inventory exactly includes all **3,888 regular files recursively under fifteen gate roots**: twelve completed and three failed attempts. It also preserves the original independently inventoried inputs, named runtimes, current binaries, historical source snapshots and explicitly scoped supplementary artifacts. Logical bytes count paths, including hard-linked/copy-equivalent content, and are not a physical storage measurement.

**2,001 inventoried paths / 3,941,363,748 logical bytes remain local-only.** This includes large Git bodies, fixture repositories, stopped R2/SQLite state and ELF binaries. The capsule separately lists all 121 excluded large top-level payloads with correct local hashes/sizes and explicitly excludes nested fixture/state payloads. It does not claim that the approximately 3.95 GB inventory payload is published. Historical overwritten Rust-runner binaries and unavailable source versions are identified by their original maps with availability flags; the flags match the retained files and do not fabricate missing payloads.

## Review, gate and accounting continuity

The published source review is byte-exact to the original 11,243-byte independent report. Its capsule copy is also exact. The original **647,042-byte / 2,129-file** inventory is preserved unchanged in the capsule with SHA256 `28dc32a7dca90b9379b3f83dc695841a01307a74213770a585aff8e3d9aedc95`. Every one of its records was checked again against the newly streamed local file identities. The expanded author inventory does not silently amend the scope of that original review.

All twelve complete gate maps remain byte-identical before/after, and all ledger record/log hashes match their raw files and capsule copies. I revalidated the historical whole-file source hashes and production-prefix comparison: only trailing cfg(test) helper/coverage and the exact two-line explanatory comment differ across gates; no executable line is normalized. Production Worker files remain identical. The three failed runtime scopes still have their before maps/logs and lack after maps, both locally and in the capsule. Earlier canceled, sandbox and no-run diagnostics remain described as transcript evidence rather than invented standalone files.

The three matched correctness-pair summaries match the original independently checked records, including the final 18-row/11-response pair and its GET/read-byte totals. Final totals independently match Worker 70, DO 77, SQL reads 109, SQL writes 17 and the stated write/live/peak/SQLite guards in both controls. The **24.92/39.41 values are correctness-test seconds**, not benchmark latency or a speed comparison. Allocation and in-flight pressure numbers agree with raw records and retain the distinction between cache-domain ownership, retained audit Values and transient request peaks.

All 38 fault rows remain byte-exact. Unknown SQL columns remain NULL once observed, and missing-receipt accounting remains incomplete through **63 chronological before/after/follow states**. No unknown is replaced with zero or an inherited known total. Worker CPU, DO duration, Native compute and full bill remain NULL in the ledger. The 156-GET / $0.05616 figure is explicitly conditional, with no measured net benefit inferred. Fresh screen and the root's final-pair obligations remain outstanding at this R18 publication boundary.

The original 408-byte cleanup record is unchanged locally, publicly and inside the capsule. It records no owned processes/FDs/listeners on 8841/8842. Read-only `/proc/net/tcp` and `/proc/net/tcp6` checks during this review also found no listeners on those two ports. Named Node/Wrangler/workerd identities match their retained files and remain explicitly **post-gate identities**, without invented historical before/after proof.

## Verification artifacts and limits

Full member and local-file results are in `/tmp/git-zero-protected-chunk-ledger-verification.json`: **1,837,016 bytes**, SHA256 `ac80e0c09b19786d7fc515862f6f9e6cfe34b9138b63f14e25f66b2920c138d0`. It records all counts, paths, sizes, SHA256 identities and explicit publication/availability limits. Reproduction script: `/tmp/git-zero-protected-chunk-ledger-verify.py`, **17,356 bytes**, SHA256 `3c782b63bca55f77e38a02c76e04bbe351397faafd43dc432fe6c0b2cda79e54`.

The source approval remains limited to its exact commit and earlier source report. This ledger review verifies present local bytes and faithful compact publication; it does not create missing historical artifacts, retroactively prove filesystem immutability, turn post-gate runtime hashes into historical runtime proof, publish the local-only payloads, or establish detection of arbitrary RAM corruption. No cost/performance, promotion, acceptance, global floor, plateau or stopping conclusion follows from these local correctness gates.
