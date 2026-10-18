# Independent R16 small-receive design review

**Conditional design approval only.** The proposed optimization is technically viable with the narrow prepared-receive change and conditions below. Simply feeding the R15 fetch continuation into ordinary receive is incorrect: it duplicates Full snapshot reads and can misclassify metrics. There is no approved R16 source, measured R14/R15/R16 savings, latency acceptance, or stopping decision. Implementation must start from the exact frozen independently reviewed R15 commit, followed by actual production-Git controls and the authorized two-pair screen.

This was an independent read/offline review. No repository source changes, builds, services, probes, benchmark runs or CI actions were performed. Python was used solely to inspect retained JSON, calculate arithmetic and write this review. No MJS runner was created or used.

## Independent receipt check and attempts to falsify the price hypothesis

I read the fresh run `39097951585` reports directly, checked all four R13 report hashes against the replay input, and indexed their 704 measured operations by profile/sample/operation key. Each profile/sample contains 88 operations and 24 small pushes. I separately matched **all 96 structurally eligible cold small-push rows** (48 per four-profile replay, reproduced across both samples) against original report wire bytes, actual workload GET/read-byte/Worker/DO receipts and a positive exactly-one cold rehydration counter. No warm row is counted as eligible.

| Profile | Eligible per sample | Actual small-push wire bytes | First-batch GETs summed over those pushes |
| --- | ---: | ---: | ---: |
| 16 MiB cold | 24 | 712–719 | 51 |
| 64 MiB cold | 24 | 715–722 | 24 |
| Either warm | 0 | 712–722 across sizes | 0 |

The real opportunity is **one existing service Worker + one existing DO callback removed per successful prepared push**, while all GETs/bytes remain. With frozen rates and 1,000 repetitions, `48 × (.30 + .15) × 1000 / 1,000,000 = $0.0216` gross. At the actual R13 $1.80461843618 anchor this is only 1.1969%, with $0.0035538156382 overhead headroom before falling below 1%. At that anchor, fewer than 41 successful callback eliminations out of 48 cannot reach 1% even with zero known overhead. Recalculate against the actual preceding candidate score; the illustrative post-R15 denominator is not a measured result.

I tried to falsify eligibility using the actual wire sizes, initial/large-push exclusions, zero-GET warm traces, callback presence and scope. None of those rules eliminates this cohort. However, retained reports do not contain complete original HTTP bodies/headers. The 48 count proves a structural cohort, **not** all future pkt-line/MIME/content-length/capability classifier branches. A valid strict classifier that activates for too few real small pushes, any hidden second reconstruction, extra service round trips, or costly fallback invalidates the >=1% hypothesis. Fresh on/off real-Git controls must establish activation without fixture-specific exceptions.

This is a request-price hypothesis. Added crypto, framing and Native round trips can regress latency or unmeasured compute. Worker CPU, DO duration, Native compute and monthly SQLite retention remain unknown; a full bill remains NULL. No global API/layout lower bound follows from these receipts.

## Required prepared-receive structure

1. **Mandatory fix for the unconditional cold reread.** Current `native.rs:1027` calls `cloud_read_view()` unconditionally; that function rebuilds again even if scope is already Full. Add an internal, non-Clone, per-call `ValidatedFullReceive` argument consumed by value through receive/receive_transaction. Only the authenticated continuation may construct it, after verifying and materializing the entire exact Full snapshot, all remaining ordinary batches included. Ordinary `None` receive must retain its current cold policy. Do not replace this with global pending state, a broad `require_full_view()` call, or trusting scope alone.
2. The token binds the current generation, paired published/local manifest fingerprints, exact Full descriptor/selection identity, logical request fingerprint, registration session and continuation nonce. Its ownership is stack-local, with bounded metadata only. Recheck Full scope, local state and binding at entry before skipping that one preparation step. No byte cache, spool, session map or public way to mint/reuse the token is introduced. The plan must select **all** packs, never a history prefix or have closure.
3. Keep the existing receive wrapper and transaction: certificate/lineage transfer, pending metadata charges, `publication_active`, debt cleanup, exact refs, quarantine/alternates, strict receive-pack and thin-pack completion, bounded compaction/import, existing R14 PUT/coupled manifest-sync/retirement ordering, post-publication cache invalidation and dirty recovery. Planning performs none of these mutations and runs no Git codec.
4. Preparation timing must include prefetched Full materialization instead of starting only after it has completed. Count one logical push request and its input/output exactly once. Set service phase **push before remaining reads**, retain actual first-dispatch receipts under push, and do not increment fetch cache misses or insert a receive response into the fetch cache. Maintain the actual cloud rehydrate/Full-restore counters once, including starts from a projection.
5. The private handler currently bypasses ordinary `handle` error cleanup. A receive continuation must explicitly preserve the same cleanup semantics: invalidate advertisement/lineage as applicable and preserve dirty/invalid state after attempted preparation, cleanup, codec/publication or local-filesystem failures. Reuse a factored common error handler or demonstrate equivalent behavior; do not assume fetch-only errors cover receiving. Ambiguous publication/receive must never be retried by the gateway. Lost committed replies still count their actual attempted work.

## Classifier, authority and authentication conditions

- Add a distinct exact receive capability dependent on strict first-read + witness registration. Old R15 clients remain fetch-only; warm providers opt out. A present malformed capability, incompatible dependency/session or unsolicited opt-in must fail strictly and reset capabilities before service I/O. No persistent format should depend on negotiation.
- Classify exact POST `/repo.git/git-receive-pack`, correct MIME, bounded declared <=128-KiB body, identity encoding, allowed protocol, no unsupported transfer framing, and bounded canonical headers. Reuse standard Git eligibility rules where applicable. The authenticated logical request target/purpose must distinguish fetch from receive; bind original bytes, method/target, semantic headers and session/nonce/hash. Cross-purpose replay or a fetch envelope accepted as receive is a blocker.
- Before authority-plus-first-GET, conservatively parse the supported command pkt-lines, flush and nonempty PACK prefix. Exact `0000`, no-PACK/ref-only requests, unsupported valid grammar and bodies above the bound must fall back **before R2 I/O** to the ordinary productive receive path. Malformed Git must still receive normal strict Git handling. Keep original semantic headers even where transport fields are excluded from fingerprints/cache keys. Never weaken Authorization/Cookie/Git-Protocol/content encoding handling.
- Plan only from clean known authority state, with complete validated snapshot metadata. DO atomically reads authoritative generation and compares the **published** fingerprint before GET. Native re-derives the exact Full selection/local fingerprint before consuming verified first chunks once. Keep R15's paired fingerprint: durable garbage remains in the published manifest after actual retirement while local garbage is cleared. Using only the local digest fails immediately after compaction.
- A generation mismatch returns zero stale-plan GET bytes and enters an explicitly charged ordinary path before mutation. Local state change after paid prefetch discards and charges those bytes before safe fallback. Corrupt/truncated/auth-invalid frames are errors, not hidden retries. Recheck registration/session identity at every async boundary. Accept the DO receipt exactly once before later authentication/body processing can fail.
- Retain every existing cap: 128-KiB original body; 1,500,000-byte metadata; first bulk <=16 MiB payload/256 entries plus 2,060 bytes frame overhead; complete continuation maximum **18,410,368 bytes**, within the existing 64-MiB incoming bound; snapshot <=128 MiB; joint owning metadata/cache budget 32 MiB. Additional capability/key/fingerprint storage is counted. No large body plus batch combination and no cap raise.

## Required actual controls before source approval and pricing

- Same-source real-Git on/off paired small thin pushes, starting Full and projected: identical exact resulting refs, strict fsck/archive, actual Full GETs/read bytes, SQL/R2 write counts and bytes; exactly one fewer Worker/DO callback and exactly one cold reconstruction. Repeat a small push immediately after actual generation-five retirement, then read latest/history.
- Genuine default large push including its `0000` probe, explicit empty probes from Full/projection, no-PACK/ref-only and valid unsupported grammar, warm and missing/old capability: all productive ordinary behavior, no unexpected prefetch/extra GETs.
- Current-generation and local-state change races, key/session replacement, unsupported/malformed capability, signed/auth/frame/request-purpose/nonce/hash failures, truncated/corrupt verified chunks, duplicate and canonical headers and maximum bounds. Actual failed/paid receipts and fallback classifications must be retained.
- Receiving failures after actual PUT, coupled CAS, sync and retirement; partial/unknown retirement, lost committed response, same-key replay, then subsequent mutation. Verify dirty recovery and no second ambiguous receive attempt. Check all actual phase receipts and preserved owning-cache/SQLite/object-storage bounds.

These controls are correctness/topology gates, not cost or speed measurements. Review the exact committed R16 source and evidence independently, then measure fresh net cost/latency in all four profiles. No promotion or plateau claim is justified by this review.

## Exact reviewed inputs and mutable source identity

The R15 checkout was mutable; hashes below identify the source inspected, not an approved/frozen commit. R16 implementation is conditional on a separately frozen reviewed R15 SHA.

- `/tmp/git-zero-r13-actual-cost-and-small-push-design.md` SHA256 `d4dc39fe4b141bed8f1d684d25f4d22027597136af437c81100a328f75b71402` (17203 bytes).
- `/tmp/git-zero-r13-actual-cost-and-small-push-design-data.json` SHA256 `aa232fef2ea687b400cefb33f4da13be8af9ef71a13b3aa7ea1cf9f9457cd7de` (915376 bytes).

| Source under `/tmp/git-zero-hosting-gateway-first-read` | SHA256 | Bytes |
| --- | --- | ---: |
| `examples/repository-host/src/first_read.rs` | `6eda5528604f4086fa921af04ed9e1d5204e4ddde62ddfcb24d65226e9492fdb` | 22360 |
| `examples/repository-host/src/native.rs` | `838ff4672d8ceabbb0a04c4d8efc40f6fe60aa1153c25f008eef2e2604780874` | 103312 |
| `examples/repository-host/src/storage.rs` | `31cb3e8bdba18ee7f48f951164763e0d1c536192f685ca21179cd8166a36d8c0` | 50248 |
| `examples/repository-host/src/cloudflare.rs` | `e36fe507b12858da91f5145cacddf4359fd454a30c3391017361118cd24f6a94` | 100246 |
| `examples/repository-host/cloudflare/src/first-read.js` | `8c34ee1fda72108ea795ed03d5f8bad4f4818a23c93dc325390f85c7ff978ffa` | 11062 |
| `examples/repository-host/cloudflare/src/index.js` | `99aeac68f8020c649d7b9c0b0fe98963f27d82432efe957c6b033ecc25449e52` | 89185 |
| `examples/repository-host/src/native/history.rs` | `b30f3432e3d6b09d872fd0d10271c90b9398d75a335bb1a1775a1c7449e66d93` | 21101 |
| `examples/repository-host/src/native/advertisement.rs` | `f4f506457de44c986033f10959db39ae4b48dad7e308d0c56fcbdea8295c972e` | 12984 |

Review written at 2026-10-18T05:51:14.266707+00:00. **STOP READY.**
