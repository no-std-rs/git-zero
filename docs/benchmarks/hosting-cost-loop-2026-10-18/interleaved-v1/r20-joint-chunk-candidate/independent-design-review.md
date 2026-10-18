# Independent review: actual joint-budget chunk admission

**CONDITIONALLY APPROVED — DESIGN ONLY. No code GO.** The proposed `Joint` variant is a credible, minimal next experiment: remove only the unallocated secondary-headroom admission predicate from independently approved R19 source `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`. Keep explicit current `multi` as the same-source control, `on`/`one` as ONE, and make `joint` the new cold-Cloudflare candidate default only after implementation approval. I found no blocking design defect. Implementation still requires explicit ROOTGO; source approval, local production gates, ledger review and fresh measurement remain separate.

This was independent offline work. No repository/worktree/source was edited, no build, host, probe, production runner, benchmark, push, CI or deployment was performed, and no agent was delegated. Only this review and its JSON evidence were written under `/tmp`. I read the immutable source rather than relying on the mutable checkout. The read-only implementation preparation is consistent with this verdict and must remain STOP READY until ROOTGO.

## What the source supports

In `native.rs:1873–1919`, a candidate must pass the actual `non_chunk_bytes + chunk_needed <= 32 MiB` check. For a nonempty pool it must also pass `chunk_needed <= 17 MiB`, because the existing policy subtracts an **unallocated 15 MiB** heuristic. `chunk_needed` includes all chunk owners and conservative old/new backing overlap; the 17 MiB ceiling is not a secondary-payload allowance. The first anchor is exempt only from that heuristic, never from the actual joint check.

Joint should omit exactly the extra nonempty-pool predicate. Preserve every other eligibility, charge, limit and behavior:

- Required, already-paid, successful verified GET misses are considered in selected order. Admission does not evict existing chunks or responses, does not stop after the first 32 candidates, and admits nothing from PUT, startup, observer, stats, planning or warm setup.
- The retained cache domain remains at most **32 MiB**, with at most **32 chunks inside 256 combined entries**. Individual chunks and retained responses keep their **16 MiB** eligibility limits. There is no 14 MiB response cap and no new owned reservation.
- Required response/control allocation removes newest secondaries through the anchor before response LRU. Empty pools release backing; partial prune/eviction retains and charges capacity. Keep preallocation old-plus-new backing checks.
- Prune only against installed, authoritative complete current Full state after strict confirmed publication or ordinary authoritative Full recovery. Projection/proposal/ambiguous CAS/observer reconstruction does not authorize prune. Survivor order and oldest-survivor promotion stay intact.
- Keep exact descriptor hash/length/current membership/order, bounded resolver, immutable handles, opaque authenticated plan and mask, stable one-use Full receive token, receipt-once accounting, safe fallback, CAS/delete/GC/restart semantics and all service caps. No authority, selection, storage layout, billing or workload change belongs in this optimization.

The private `VerifiedChunk` owns one individual Vec, and `retained_bytes()` charges its actual payload/key capacities, private data, Arc counters and allowance. `cloudflare.rs:352–389` copies each verified decoded bulk payload into its own Vec; a handle does not secretly retain a whole bulk frame. A legacy chunk above 16 MiB remains uncacheable even if its scalar GET is required. These source properties support the design, while actual allocator behavior and transient reconstruction copies still need measurement.

## Independent reconstruction and replay

I rebuilt the ordered selections from the **original fresh R18 reports** under run `39113545149`, their fixture schedules, actual uploaded lengths, append/compaction order and trusted history floors. I did not execute or import the author's replay function. I independently simulated ONE's frozen resident partition for each reconstruction, then separately implemented the two MULTI policies.

All **704 priced rows** match actual GET attempts, read bytes and ONE hits: four profiles × two samples × 88 rows. All 32 push object inventories per trial match reconstructed live object counts and bytes; all eight final ordered pack-length sets match. The reconstructed ordered selections also match every audit row. All 14 declared source blob SHA256/size identities match immutable Git objects; I additionally read and pinned CLI and Cloudflare source. I verified the source-review report/inventory hashes, without claiming a new exhaustive audit of its 2,375-record inventory.

The independent pool replay includes response ownership/entry boundaries, the initial discovery insertion between the first two reconstructions, frozen selected partitions, first-fit admission, newest-first pressure, confirmed Full prune, retained nonempty capacity and old-plus-new growth. It reproduces **every default per-row GET count and resident set**, all aggregate fields and all four pressure sensitivities, with no disagreements.

| Cold profile, each sample | Fresh measured R18 ONE GETs | Conditional current MULTI | Conditional Joint | Further GETs avoided | Read bytes avoided |
| --- | ---: | ---: | ---: | ---: | ---: |
| 16 MiB | 346 | 153 | 85 | 68 | 349,321,542 |
| 64 MiB | 506 | 354 | 229 | 125 | 600,896,798 |
| Four-profile total, warm contributes zero | 852 | 507 | 314 | 193 | 950,218,340 |

The two samples independently agree. They validate reproducibility; their savings are not added together when pricing one four-profile repetition. Actual warm GETs/hits/admissions and both warm policy results remain zero.

This is **not a resident-set superset result**. Per cold trace I reproduce 12/10 rows with a current-MULTI resident missing from Joint, and four rows where Joint performs more GETs. Those rows are included in the net result. For 16 MiB, pushes 17/21/25/29 change 1→4, 2→5, 3→5, 4→6 GETs; for 64 MiB they change 4→7, 5→6, 6→8, 7→9. Joint has 28/29 pressure evictions and 21 confirmed prunes. Its maximum modeled chunk ownership is 30,524,426/30,778,327 B, and maximum joint admission charge including growth overlap is 31,578,282/31,832,145 B under the 1 MiB analytical stress input.

I also directly checked prior actual R19 local contracts as counterexamples to permanent protection: 16 MiB latest-recheck/history at generation 12 reduces six residents to two to zero as response ownership reaches 25,385,865 B; 64 MiB latest-recheck reduces five to zero. The actual initial 64 MiB response is 33,832,872 B and retains no response entry. These are existing ONE/MULTI correctness evidence, not Joint savings or newly executed tests.

## What remains conditional

The 399 B per-handle model follows 64-bit source layout plus a 71 B key, 8 B slots and a 256 B nonempty backing allowance. It assumes payload capacity equals serialized length and `reserve_exact` yields the requested capacity. Production charges actual capacities. The stress input is an analytical allowance, not retained memory or a proposed reservation. Fresh R18 boundary control ownership is only **263,389–264,483 B**; boundaries do not establish in-request socket/certificate/pending-publication/control peaks.

The replay holds measured response boundaries and nonchunk behavior fixed; it does not execute response cloning/ring-growth or the complete control-allocation timeline. Cached responses can consume almost all 32 MiB and evict every chunk. Transient verified batches, held immutable handles, reconstruction copies, legacy larger requests and allocator/request peaks can also change resource outcomes. These gaps can reduce or erase benefit despite correct admission safety.

The archive has no complete continuous per-GET key timeline. Matching every count, byte total, hit count and pack set strongly constrains the source-derived reconstruction, but does not turn it into an observed cache-event trace. DO-instance recent-key histograms cannot establish ordered admission or bridge epochs. Changed missing partitions can alter preparation support, authority fallback, paid discards, Upgrade/rotation and request counts; zero extra paid callbacks is not proved.

Using the frozen dated `cost.rs` rate, **193 × 1,000 × $0.36/1,000,000 = $0.06948** conditional Class B reduction, with no DO/Worker/SQL/storage/read-byte credit. The 0.5/1/2 MiB sensitivities all reproduce 193 GETs; 4 MiB gives 143 GETs and **$0.05148**. The $0.06948 gross opportunity exceeds 1% of a preceding known objective below $6.948 before added costs. It is about 4.88% of measured R18's $1.42325543618 solely for scale. **R19's full-screen denominator is NULL/unmeasured.** Zero positive unconditional savings are credited; actual net saving may be zero or negative.

R18 is a provisional known-cost anchor, with the fresh comparison inconclusive and plateau-ineligible. The earlier narrower bootstrap opportunity remains unimplemented and can reasonably be deferred for this smaller-source-surface, larger conditional experiment. No optimum, global floor, winner, acceptance or stopping claim follows. Full bill remains **NULL**.

## Required first gate and later acceptance

After explicit ROOTGO, use a **Rust** runner through real production Native/workerd and the unchanged Worker/config. Compare explicit **current `multi` versus `joint` on the same new source/binary**, for both 16/64 MiB, with fresh Native/cache/client roots and identical durably sealed input copies. Previous ONE/MULTI pairs cannot isolate headroom. Charge actual startup GETs/read bytes/authority separately while proving zero startup admissions; no out-of-timer cache prewarm. Before/after source/binary/Worker/config maps and all failed/exploratory attempts must remain available.

1. Include real small and large replacement pushes and compaction, immediately following latest reads, latest rechecks, saved random historical no-have and valid-have reads, and a true external-base thin receive. Prove a real headroom-only rejection in Multi followed by useful Joint admission/reuse, without requiring the model's exact count. Cold latest misses must remain naturally positive. Preserve byte-identical logical bodies, strict index-pack/fsck/archive, manifests/refs and mutation outcomes. Include a real >16 MiB uncacheable response and natural chunk hits.
2. Count actual owning allocations and separate request/process peaks. Exercise a productive **>14 MiB and <=16 MiB** response and exact 16 MiB boundary, full-size chunk, existing responses, 32/256 limits, key/header/ring/pool capacities, nonempty backing, old/new growth, actual socket/certificate/pending/control pressure, long first-fit scans and newest-secondary-through-anchor pressure before response LRU. Reject rather than replacing live owners during admission. Test metadata reservation before allocation; do not relabel transient heap or saved audit Values as retained cache ownership.
3. Close actual GET attempts/read bytes and cache decisions alongside Worker gateway/service/observer, DO, SQL reads/writes/SQLite, Upgrade/rotation, paid discard/fallback, PUT/DELETE/LIST/CAS outcomes, written/live/peak objects and bytes. Preserve unknown SQL/incomplete receipt status and setup/evidence/ordinary phase attribution. Equal writes/storage need direct proof.
4. Repeat held authenticated-mask pressure and paid fallback, stable Full token through legitimate admission/prune, full descriptor/resolver/corruption controls, projection and observer no-prune, confirmed prune/promotion/refill, stale/lost/partial publication, no ambiguous replay, SAME-Native physical GC before the next PUT, same-root restart, warm zero pool and ordinary HTTP. Retain unchanged authority and 16-RPC rotation.

Any invariant/cap/write/storage/resource/latency guard regression blocks progression. Defer if actual ownership prevents reuse, hidden pressure erases the gain, or extra paid callbacks remove the required margin. A successful local gate still needs independent exact-source/ownership/accounting/fault review and ledger review before a **fresh interleaved two-pair, all-four-profile cloud screen**. The mandatory **five fresh original/final pairs** remain undone. Root controls implementation, integration, push and subsequent measurement ordering.

## Sealed provenance

Reviewed audit Markdown: 17,311 B / SHA256 `e845c488f97aa76cdd82af9403cb5e9503bf54e1f7d2aafd76b9abb71bfe9b63`; audit JSON: 1,834,013 B / `255f4d1ce511e71d7031c5c26963701b78d5776942a5c993466bb1aaa631c6df`.

Exact R19 SOURCE ONLY review: 13,975 B / `f9d63594e4ae26e97afcfdbbde7ffac0b1138b591563cf258ad1352b9212fb1a`; its inventory: 463,987 B / `b17e31d26393aef40e9db3f0f04126470b0a8e1484bc4faf4fd13ce5895c92cc`. Native source at f124: 134,028 B / `a0a70c78b98a814474bf060f76dabee6e0acbd3c0cd0cc3d1d25758353ce0d1b`.

Read-only preparation Markdown: 17,637 B / `187b0452748bc813e234dc53242fd13b08aa13b7362451d46079a04d5046f71d`; JSON: 17,384 B / `280ddf3f0a8352985c24ce0cf9299a1b0285b6a9e3711cf08ef3a673f1cde62f`.

The companion `/tmp/git-zero-cache-headroom-independent-design-review.json` is **2,372,648 B / SHA256 `5e39abc14938e05e9e2ad836f510d5e96ae9adbbbcc37eb3a7748cc1637ec969`**. It contains exact source/report/input identities, all independent row results/resident sets/pressure events, sensitivities, the self-contained independent verification program, caveats and required gates. All four original fresh R18 report hashes and the exact known-score/comparison hashes are retained there. No code or runtime GO is granted by either artifact.
