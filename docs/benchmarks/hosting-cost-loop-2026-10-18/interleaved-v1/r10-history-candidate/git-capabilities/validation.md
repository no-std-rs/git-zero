# Real Git capability correction: isolated correctness evidence

The frozen R10/R11 screen at run `39092442019` performed zero historical projections. The native advertisement includes `deepen-since` and `deepen-not`, and Git 2.53.0 echoes both on ordinary protocol-v0 fetches even without shallow options. R10's strict request classifier omitted both. The two-token correction accepts these capability acknowledgments; actual shallow/deepen/filter commands, unknown wants/haves, external generations, integrity checks, provenance closure, and memory bounds retain their existing behavior.

This is correctness evidence, not a performance measurement or accepted cost saving. The original failed screen remains unchanged. Integration, promotion, and a separately declared fresh screen require independent review and the current screen's four-profile guard review.

## Frozen source identities

- Base and untouched primary HEAD: `7e7856a3f6c279a8f97956a4475cb12f847b1f7e`.
- Isolated branch: `perf/hosting-history-git-caps`; implementation commit `4b777f32b8b443602406b30fa16f949d6896a2fb`.
- Entire Native Rust `examples/repository-host/src` Git tree: `3b81fc646d78eedb09646bce7cdc24ea90feb313`.
- Native submodule directory Git tree `src/native`: `7d4e8f1bcc4532b803587292a61c74b7050898f0`.
- Unchanged Native integration file `src/native.rs` SHA-256: `2d4a4ab13b152ad2be2a02d0648685678765134831958207e4b7445f4169faa3`.
- Whole unchanged Worker `cloudflare/src` Git tree: `ca33ec32a85efab36f552a6d415aeca7ee5ba839`.
- Worker `index.js` Git blob: `2cdb3f240216ea66481ca0a712bbaaf899234178`; SHA-256: `f5f72b40ec605814be6a6a0311df44e255ffdc3b402afec2e66454aa5711a811`.
- Unfixed `history.rs` SHA-256: `d37f9721f90eab0919c349bcd285dd9acfa42ba25834e3c105475d29c87d7e44`; corrected SHA-256: `2eb3b7704487ebe93d47fa82a120076cce0ebb90a71376344f110cc9ac208a3c`.
- Debug Native executable SHA-256 before: `f5b822f7ff5e2147f778565e4acc793ac94aff57aaada7810e46cc6d62eec6ef`; after: `77f686bab9628755f424a5f86230f2217eddfca27c871870d313a61a3e8cea46`.
- Identical Rust runner source used for both completed runtime runs: SHA-256 `f68ac11041f2bd2a54611afed6d46bc55cb33b881f785f3fe49463b87e0e0ba1`; preserved as `tested-runner.rs.gz`.
- Final portable Rust runner SHA-256: `fa1c9f8253f732fb4e11c0729274c0f5aa9ba7459fac2e013087cc138a094972`. After both runtime runs, its fallback CLI path alone changed from the installed shared CLI to `worker.join("node_modules/wrangler/bin/wrangler.js")`, retaining `GIT_ZERO_HISTORY_WRANGLER` as an override. Formatting and all-target package Clippy passed again; no runtime repeat was needed for that path-only adjustment.
- The post-fix runtime test executable SHA-256 was `4c5a4c4f21c0d1391fe28ee365eaac712481f6ec94935c680d593934452ffd3b`. A pre-fix runtime test executable hash was not captured before replacement; the unchanged tested source and both Native executable hashes are preserved above.

## Actual-client receipts

The Rust integration driver owns a fresh production Worker on port 8834 and a registered Native codec on an OS-assigned port, using separate fresh persisted state for each run. Both clients use real installed Git 2.53.0, protocol v0, `fetch --quiet --no-tags --keep`, and `fetch.unpackLimit=1`. The second client is seeded locally with an older published commit. Packet tracing is set only on the client process; Native sees the four actual CI policy variables: `GIT_CONFIG_COUNT=0`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, and `GIT_CONFIG_GLOBAL=/dev/null`.

| Case | Unfixed read/full bytes | Unfixed projection increment | Corrected read/full bytes | Corrected selected packs | Corrected projection increment |
| --- | ---: | ---: | ---: | ---: | ---: |
| Real Git, no haves | 9,448,323 / 9,448,323 | 0 | 9,443,988 / 9,448,325 | 2 / 5 | 1 |
| Real Git, older seeded have | 9,448,323 / 9,448,323 | 0 | 9,443,988 / 9,448,325 | 2 / 5 | 1 |

All four requests passed wanted-ref checks, strict full fsck, archive equality, and zero authoritative R2 writes / SQL row writes. Both request traces include `deepen-since deepen-not agent=git/2.53.0-Linux`; the seeded trace includes the exact older have. The separate runs generate fresh commit timestamps, accounting for their slightly different full byte totals. No timing or savings inference is made from these fixtures.

The unfixed run exited 101 at the expected assertion: `real Git historical requests must select fewer R2 bytes; inspect real-*-receipt.json`. It saved both receipts before asserting. The corrected complete production contract passed, including exact response-cache hits while a Projection remains active, metrics without hidden restoration, full stats restoration, the default streamed >8 MiB push with empty receive probe, full restoration before nonempty receive, final strict fsck/archive, and final inventory restoration. Active cache caps remain 33,554,432 bytes / 256 entries.

## Validation ledger

- Focused `cargo test -p git-zero-host --lib native::history --offline -j2`: 12 passed, including realistic capabilities with/without haves, negative standalone shallow/deepen/filter commands in both phases, unknown wants/haves, external-generation recovery, integrity, and memory contracts.
- Unfixed production driver: expected 101 activation failure, both real-client receipts retained.
- Corrected production driver: 1 passed, 0 failed; correctness-only runtime 59.32 s, with no benchmark claim.
- `cargo clippy -p git-zero-host --all-targets --offline -j2 -- -D warnings`: passed, repeated after portable-path adjustment.
- `cargo fmt --package git-zero-host --check` and `git diff --check`: passed.
- Both Rust-owned process groups exited after each run. A separate host check after completion found no listener on port 8834 (`ss -ltnp 'sport = :8834'`, header only), and no process matching the owned `/tmp/git-zero-history-git-caps-(after|target)/` paths (`pgrep`, exit 1). The primary was rechecked clean at its original HEAD. No source changes, pushes, CI changes, or benchmark services touched that primary.

Actual runtime command environment included `PATH=/tmp/git-zero-upstream-install-2.53.0/bin:$PATH`, `CARGO_TARGET_DIR=/tmp/git-zero-history-git-caps-target`, the four Git policy variables above, `GIT_ZERO_HISTORY_OWN_RUNTIME=1`, `GIT_ZERO_HISTORY_CLOUDFLARE_ORIGIN=http://127.0.0.1:8834`, and fresh `GIT_ZERO_HISTORY_ARTIFACTS=/tmp/git-zero-history-git-caps-{before,after}`. Command: `cargo test -p git-zero-host --test history_cloudflare production_history_prefix_contract --offline -j2 -- --ignored --exact --nocapture`.

Both completed runs resolved `/tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js` through the then-default path. That file's SHA-256 is `780661a508810f3b65786895b1ca9aacbc4f55d329ae6b8c1e49ec8433569f77`. Repeating with the final portable runner and this shared install requires the explicit `GIT_ZERO_HISTORY_WRANGLER` override set to that exact path.

## Retained evidence

- Raw before directory: `/tmp/git-zero-history-git-caps-before`, raw after directory: `/tmp/git-zero-history-git-caps-after`. Each preserves real-client receipts, packet traces, Native/Worker logs, and client repositories. The after directory also preserves the full `contract.json`.
- Assembled raw before report: `/tmp/git-zero-history-git-caps-before/report.json`, SHA-256 `c7bdd81fe60a2c132bc15d4dce00a056ffaa2f1c314022a00702a6fe2dc63eaa`.
- Assembled raw after report: `/tmp/git-zero-history-git-caps-after/report.json`, SHA-256 `cdd4d22255f3faef26802f21a4b29e99ed71353e12d00b0823792dccf2846ef3`.
- `before-report.json.gz`, `after-report.json.gz`, `tested-runner.rs.gz`, and `correctness-logs.tar.gz` preserve the compact review evidence beside this ledger. `SHA256SUMS` covers these files and this ledger.
- Initial read-only diagnosis remains `/tmp/git-zero-history-screen-diagnosis.md`. This correction does not replace the frozen screen or select a baseline.
