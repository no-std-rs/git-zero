# Independent R19 multi-chunk design review

**Conditionally viable next design, ahead of the narrower schema hypothesis.** The supplied model has a credible material ClassB opportunity beyond ONE, but its fixed partitions cannot silently become new response caps or fictitious retained allocations. Source implementation must wait for the exact independently approved R18 base. This review approves neither mutable R18 source nor R19 implementation, measured savings, acceptance or stopping.

This was read/offline work only: no source changes, builds, services, probes, benchmarks, push, CI, deployment or delegation. The current running R15/R16/R17 screen and primary were not read or touched. Mutable R18 implementation was not read. No MJS was created or used.

## Independent replay and its limits

Input hashes match root's exact files: audit MD `c5c0805510955c6da0a07da1eb6a38786b9f1b5457048df25604c380d7810b99`, JSON `32d49852b8a85517ac828cd1d105a3c1f937576a9bc3b9834ea510b453d41b96`. I independently read the four **fresh complete R15 reports** from valid run39106495382, checked every report hash against the audit, and matched all704 measured operation keys/counts and actual GET/read-byte totals across eight trials. I then independently replayed the modeled ordered selections, first-fit admission, slot charge and confirmed-push pruning, without executing or overwriting the author's calculator.

| Cold profile, each sample | Actual R15 GETs | Conditional ONE | Conditional MULTI | Additional GETs avoided | Maximum secondary payload / chunk entries |
| --- | ---: | ---: | ---: | ---: | ---: |
|16MiB|425|346|153|193|8,859,085 bytes /6|
|64MiB|585|506|361|145|767,355 bytes /4|

Both samples reproduce these model values; warm modeled admissions/GETs are zero. I also reproduced the bulk-batch totals118→72 and203→195. Consequently338×$.00036 = **$0.12168 conditional ClassB opportunity** per1,000 four-profile repetitions is arithmetically correct. The replay's first latest operation has two reconstructions, so ONE's total savings include a within-operation reuse; positive operation count alone is not complete access count.

The raw archive does not provide a continuous complete per-GET key stream. Publication/index identities, exact selected descriptors and prune events are source/readset inferences consistent with actual counts/bytes and source compaction/prefix behavior. They are not a production cache hit trace. The reserve assumptions also preserve old response hits without peak-allocation evidence. Therefore no positive unconditional net saving is proved; actual policy pressure or extra response misses can change the result. Do not publish this as a measured R18/R19 score or mix it with an incomplete or unreviewed performance attempt.

## Concrete production policy and implementation conditions

1. Extend the approved ONE cache to a common bounded mode **one versus up to32 immutable chunks**, with at most31 secondaries and all entries counted inside the existing256 combined response/chunk entries. This is a generic production feature control, independent of fixtures, key values, repository history or benchmark stage. The primary native codec still rebuilds cold views.
2. Admit only already-required, successful, exact hash/length verified GET bytes from ordinary public Git work. No PUT/import, setup/stats, external prewarm, speculative or dummy reads. If no slot fits, reject that admission; **do not evict a live cached chunk merely to admit a later scan item**. Deterministic insertion-order first-fit is sufficient; no prediction, branch/generation weighting or unbounded LRU map is needed. Protection lasts only while useful and affordable.
3. Prune cached descriptors absent from the **complete trusted current manifest after a strict confirmed winning publication**, once the authoritative full manifest is installed. Physical DELETE success and logical unreachability are separate: a partial retirement can still have a confirmed new manifest. Never prune from a projection's selected subset, proposed next manifest, stale/ambiguous CAS, observer refresh or unverified external generation. On ambiguity retain harmless old entries until ordinary recovery/pressure, and still check exact current selected membership before use. Memory-pressure eviction is a distinct explicitly counted reason, not a fabricated retirement/prune event.
4. Define the generic anchor-loss case: a pruned anchor cannot justify evicting live secondaries for a newly encountered larger item. Promote an existing eligible resident without allocating, or admit the next actual GET only when it fits under the same policy. Any such choice must be deterministic and covered; this trace keeps its initial anchor live and does not prove that case.
5. Reuse approved ONE's cache-aware resolver and authenticated plan. Build the complete stable Full/projection selection first, then exact cached descriptor **set** and first missing bounded batch. Preserve original reconstruction order and strict duplicate-key/length rules. Authenticate generation, published/local manifest binding, stable selection, cached/missing descriptors, logical request hash, nonce and session; recheck before consuming paid work. Admission/pruning after validation must not retroactively change the stable Full receive token or trigger mutable post-admission plan revalidation. Eviction/local state change between plan/continue discards and charges prefetched bytes before explicit safe ordinary fallback.
6. Preserve authority for all-cache reads using approved ONE's ordinary authority fallback unless a separately explicit versioned empty-batch protocol is independently approved. All-cache does not permit a stale witness, empty fabricated R2 proof or dummy GET. Current service caps, R17's16-RPC rotation, receipt-once ordering, ambiguous-no-retry contract, receive quarantine/thin-pack validation, publication/retirement ordering and shared error cleanup remain intact.

## Memory/response conditions that falsify a careless implementation

**14MiB/1MiB in the audit are modeling assumptions, not new caps or allocations.** Existing responses up to the16MiB entry limit must remain eligible by reclaiming chunk storage under real pressure. A hard14MiB response ceiling would change the API/cache policy beyond this proposal; an idle14MiB reservation would misreport owning memory and unnecessarily suppress generic admission. The1MiB allowance is not evidence that actual control state fits, and the256KiB socket charge applies only according to the approved actual socket accounting. Warm mode must not allocate/reserve a cold secondary pool. Use actual owning usage and existing limits; remeasure the resulting admission sequence instead of forcing the338 count.

Before response insertion, certificate/control growth, socket/register/reset changes or pending-publication allocation, reclaim secondaries and then the anchor if needed **before exceeding32MiB or256 entries**. No chunk is exempt from pressure. Required metadata takes precedence; the first-fit no-eviction-for-admission rule does not prevent eviction for real non-chunk memory needs. If exact allocations do not fit, reject a new admission or reduce residents and retain the actual hit/miss consequence. Metrics must distinguish response-only bytes/entries, chunk-owned bytes/entries, control/socket ownership and joint owning total/peak. Push response-cache zero gauges remain response-only; surviving chunks must not appear as unexplained nonzero response cache or disappear from joint accounting.

Charge whole Vec capacities, Arc/allocation headers, key storage, slot/ring capacities and metadata, not serialized lengths or arbitrary256-byte slot estimates. An Arc slice into a larger bulk buffer owns that entire parent allocation. Immutable request-local Arc clones share one physical payload and may outlive eviction for that request; they are not permission for hidden cross-request owners. Retain request-local peak evidence and avoid needless16MiB copies to satisfy old Vec APIs. The tight64MiB modeled pool has only786,432 payload bytes before slot charges: a small owning-capacity difference can reject its tail or tiny chunks. A bounded Vec/array of at most32 entries makes metadata accounting simpler than an opaque unbounded map, but capacities still count.

A cached handle remains immutable and originally verified; every use is against current validated exact digest/length and selected manifest membership. Corruption, noncached read failures and missing receipts fail safely without duplicate mutation or suppressed paid work. Cache entries cannot become storage durability/GC witnesses or skip current manifest authority.

## Callback/latency cost must be measured

54 fewer modeled bulk batches is **not**54 guaranteed fewer DO calls: R15/R16's first authority dispatch survives when all chunks are cached, and ordinary all-cache fallback changes private planning stages. Measure actual service batches, gateway authority, paid discarded preparations, R17 message counts and timed Upgrade rotations. Do not count every fewer socket message as a Worker request saved. Expanded chunk ownership can evict a large cached Git response and cause new materialization/codec cost; a GET-only improvement does not prove net cost or latency improvement.

Naturally positive latest GETs remain plausible because new published chunks are never PUT-admitted; the model's minimum latest misses are1/2 for16/64MiB. Demonstrate that property through the unchanged production workload, with no forced no-cache mode, out-of-timer warmup, dummy GET or relaxed cold guard. Random/history/push reads may legitimately become all-cache.

## First required actual production gate

After approved R18 is frozen, run **same-source ONE versus MULTI** through production workerd with the same Worker/config and real16MiB and64MiB Git fixtures. Use the generic feature control only. Include a real initial GET, more distinct4.3MiB objects than secondary capacity, repeated Full/history reads, small and large/thin receiving, genuine compaction and immediate following reads. Verify strict refs/fsck/archive, identical logical response bodies/manifests/storage writes and complete receipts. Record admissions/rejections/hits/misses, confirmed-prune versus pressure evictions, exact owning current/peaks and natural positive latest GETs. Do not require reproducing the modeled338 as a correctness condition.

Pressure/edge cases must include an existing16MiB response at the old limit, maximum actual socket/certificate/pending ownership,256 combined entries, a partial projection that must not prune hidden live chunks, duplicate descriptors, full-size chunk/Arc-parent capacity, pruning/refilling the anchor, and a long scan proving no admission thrash. Stale/lost-committed CAS, partial retirement, external generation, corrupted cache/noncached suffix, cache eviction between plan/continue and Native restart must retain safe recovery, actual costs and no ambiguous retry. PUT/setup/stats/warm admissions stay zero. Verify the stable Full receive token survives its own legitimate admissions/pruning.

These controls establish source/correctness/accounting eligibility. Independent source review and fresh all-profile measurements must establish actual net cost/latency afterward; the mandatory final five fresh original/final pairs still remain. Full bill, Worker CPU, DO duration, monthly SQLite retention and Native compute remain unknown. No global floor, acceptance, plateau or stopping follows. **STOP READY.**

## Exact inspected source and evidence

Source checkout `9135f96207e89faa5c566aaa2780991eb5644f82` differs from approved production base `c128197cc2cf1761e93832c63eec4c25c1268249` only in published R17 evidence files; production source inspected is identical. Mutable R18 source was not read.

Independent JSON: `/tmp/git-zero-multi-chunk-independent-design-review.json` SHA256 `56e8ea09e28affee279ca8408c634f0fa8e095ff1de49df83a827a4388293cdd`.

| Source under `/tmp/git-zero-hosting-socket-rpc` | SHA256 | Bytes |
| --- | --- | ---: |
| `examples/repository-host/src/native.rs` | `b8e160ad0494156279be5ab5e6c7640e642d8da55abedeeba07852703f99ec4b` | 113829 |
| `examples/repository-host/src/native/history.rs` | `b30f3432e3d6b09d872fd0d10271c90b9398d75a335bb1a1775a1c7449e66d93` | 21101 |
| `examples/repository-host/src/lib.rs` | `c16b17fa37165273d4e8ea739a898b89efebbf54f407e8a902e379dbd0650b61` | 49237 |
| `examples/repository-host/src/storage.rs` | `31cb3e8bdba18ee7f48f951164763e0d1c536192f685ca21179cd8166a36d8c0` | 50248 |
| `examples/repository-host/src/cloudflare.rs` | `3ba886c9827e5e2d5f064c899245bb19f00b03c114c63c5568086b6a3f4a34ab` | 109205 |
| `examples/repository-host/src/socket_rpc.rs` | `64d6c5b4732772bd1e14017e55103a2307e59ef2d543f7e82406579847019ef6` | 18106 |
| `examples/repository-host/src/first_read.rs` | `502d6513633fa1845f6c97c1883914c21aaa08347adb25d7bb9dd71581280947` | 25758 |

Written at 2026-10-18T08:01:41.646418+00:00.
