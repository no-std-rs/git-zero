# R14 independent source review — precommit disposition

Disposition: **correction required; otherwise conditionally ready for final SHA/evidence review**. This is a bounded read-only source and existing-artifact review. I ran no build, runtime, probe, benchmark or CI, and edited no repository files. The candidate remains uncommitted during this review, so this document is not final source approval or performance/cost acceptance.

## Exact inspected source

Worktree `/tmp/git-zero-hosting-fused-metadata`, branch `perf/hosting-fused-metadata`, base `5c616c23469fff9c7f64789dc29dc640bbc0dfc1`.

- `cloudflare/src/index.js`: `eb7bfff3883a9ceabf2de70e90de58ab7dd87eca5b4f0c77e6d94fd4b804fa58`.
- `cloudflare/src/inline-catalog.js`: `b13edf1cc493e814b1b08b406c48b5544122bbaa6130e01d71d7e6555b2aec71`.
- `src/cloudflare.rs`: `fbed40632dcf4b653d4dd8f8c795723cf235263fd7250b8c89741b8dc8b54231`.
- Existing page and proof modules are unchanged from base. Production changes are the two Worker modules and the SDK registration validation/request; no Native cache/API resource cap is changed.

I read the independent design review and post-deferral audit prep alongside constructor/reload, registration/header forwarding, promotion/demotion, generic PUT/CAS, fused PUT/CAS, debt reconciliation, ordered retirement, GC, receipt and proof issuance paths, and the new actual-binding fault wrapper and Rust controls.

## P2: validate a legacy v2 root acknowledgment before ownership transfer

`prepareInline` decodes a sole table root, selects `entries.appliedGeneration`, encodes it, optionally ALTERs, atomically transfers ownership into `repository_state`, and syncs; only then does `decodeInline(blob, state.generation)` reject an acknowledgment beyond the current generation. A valid-shaped GZCPv2 root with a future acknowledgment and a current manifest with empty garbage avoids the existing per-garbage `loadDebtContext` check. Thus the new optimization can commit a known-invalid inline owner before failing. Its final decode is outside the promotion try/catch, delaying `catalogUncertain()` until another path hits the invalid owner.

Before any promotion DDL/ownership write, compare the selected acknowledgment to the authoritative existing state generation (zero when absent), validate the complete candidate envelope with that bound, and mark invalid catalog state uncertain. Invalid metadata must remain openable for manifest reads and generation-checked physical GC. Add an actual control using the sole-table-root, empty-garbage future-ack case; ensure no physical PUT or migration SQL write occurs after detecting it, then prove GC and a normal subsequent mutation. The implementer independently identified the same issue; root and implementer were notified. All watch runtimes must stop before changing this source. Existing successful controls remain evidence for the hashes they actually executed.

## Source findings without an additional blocker

The GZIR envelope is bounded at `22 + 36*n`, at most 1,174 bytes for 32 entries. It checks exact magic/version/length, safe integer peak and generation, sorted unique digests, each member <=16 MiB and exact safe sum; decoding requires peak >= sum and acknowledgment <= manifest generation. Zero members is a persistent singleton H/ack owner. Nullable ownership is separate from nonempty GZCP page encoding. Reopen chooses representation from the durable nullable column regardless of current registration; duplicate table membership or malformed owner marks the catalog uncertain, while manifest GET and physical GC remain available.

Promotion is bounded to zero rows or one exact root row, charged, and mutation-only. Ineligible mixed/split/larger legacy encodings retain the existing path. A 33rd member and >8 KiB manifest demote with paid transaction/sync rather than narrowing the existing public API. The implementation chooses complete demotion, including a fallback peak for a zero-member root, rather than keeping a zero-member inline owner beside child pages; the ownership model remains unambiguous. Generic PUT keeps its independent catalog sync, generic CAS updates generation/manifest while preserving the root column, and capability/session loss does not strand the durable owner.

On the eligible fused path, actual immutable R2 PUT happens first. The synchronized transaction reads the current generation, always records uploaded membership/H, and changes manifest/generation only for a matching expected generation, in one actual singleton-row write. A stale fused result is returned only after sync and preserves its real uploaded orphan/current manifest; no retirement follows. A stale CAS-only path checks generation before debt resolution/demotion and performs no R2 work or SQL writes. This explicitly replaces the separate pre-CAS catalog sync with atomic row coupling and a shared pre-DELETE sync; it does not claim the old intermediate durability state still exists.

The new root is stamped with the resolved **prior** debt generation before any new-generation DELETE. Same-key resurrection takes the old mandatory pre-PUT acknowledgment/sync. Unrelated/uncoupled debt flushes first. Partial real DELETE outcomes are ordered; eager reconciliation removes only actual successes and may acknowledge the generation after recording them, while marking uncertainty and keeping failed members. Complete/partial failures do not mint an all-success proof. SQL/transaction/sync exceptions mark uncertainty; GC reloads the actual durable representation before preserving valid H plus the whole pre-delete physical inventory/orphan floor and rebuilding one owner. Observer LIST does not reconcile debt or clear arbitrary uncertainty. The existing unrecorded PUT window remains an explicit limitation, not a newly claimed crash guarantee.

The SDK asks for the exact new opt-in capability and validates all existing dependencies before installing session state. Missing capability supports old Workers. Invalid acknowledgment resets previous capability/proof/witness state before I/O and fails registration. Private headers are stripped through the existing internal-header prefix before reintroduction from registration. The private absence HMAC protocol is unchanged; actual per-RPC receipts are charged once before optional proof creation, and unknown SQL cursor counts remain null rather than zero. No full bill, CPU, duration or latency conclusion follows from this source review.

## Existing raw controls inspected

The five new actual Worker controls completed in 127.55 seconds under the inspected Worker hashes, according to `/tmp/git-zero-inline-final-worker-contracts.log`; their artifact roots are the `*-215135-*` early-shape/fallback/faults/production/realistic-storage directories. These tests are correctness/resource observations, not benchmark speed samples. The realistic root/manifest layout and reopen report 24,576 SQLite bytes; small fault repositories are 20,480 bytes and must not substitute for that storage gate.

`/tmp/git-zero-inline-native-r14-final-source-1/source.json` and `source-after.json` are byte-identical. Its six existing real-client cases have final certain, debt-empty, two-object states at 20,480 bytes. The first-cursor failure and unknown-cursor cases retain null SQL counters. I inspected the real Native report, state/usage structure, runner identity and fault-wrapper operations; I did not rerun them. Library/Clippy, all legacy controls, final real Git gateway driver, exact clean code SHA, and the evidence ledger are still pending final continuity review. In particular, no test executed after the requested future-ack correction can be attributed to an earlier hash or vice versa.

## Next review boundary

**STOP READY pending corrected clean code commit and evidence ledger.** A focused final review should verify the P2 correction/control, exact production/test source identity, every ledger artifact byte/hash and truthful intermediate-versus-final test scope. No final approval, known-cost improvement, plateau, stopping decision or faster-than-Git claim is made here.
