# Independent R18 protected immutable-chunk design review

**Conditionally viable bounded design; no implementation or measured net-cost approval.** One protected first verified GET chunk can remove meaningful repeated object reads without retaining the codec view, but the current prefix-prefetch API and post-materialization receive-plan validation must change. Those are concrete implementation blockers, not reasons to claim a plateau. R17 source remains subject to its separate reviewer; implementation must wait for its exact frozen approval. Current primary/trigger and fresh screen were not read or changed.

This was read/offline work only. No source edits, build, service, probe, performance run, primary edit, push, CI action or delegate. The supporting JSON records exact source/report/input hashes. No MJS runner was created or used.

## What the failed attempt actually supports

I directly checked the hashes and all 88 measured row identities in each of the four **complete R15 cold profile/sample reports** from failed run39104016332. Each has 79 positive-GET operations:32 latest fetches,24 small pushes,7 later large pushes,8 history reads without haves and8 with haves. Total GETs are425/585 for16/64MiB, reproduced across samples. Final manifests retain the initial first chunk of8,544,710/16,777,216 bytes. These are actual retained observations; the incomplete profile does not become a score.

Current source makes the initial-pack reuse hypothesis credible: the initial pack exceeds the8MiB compaction input limit and remains in the union; Full reads include it, and certified history prefix selection includes its early floor. The report does not archive every original R2 response/key sequence across DO epochs. Thus **one initial GET plus78 subsequent uses per cold profile is a source/readset inference**, not independent proof of every opaque response or a guaranteed hit under pressure.

If that one slot is admitted once and survives,156 GETs avoided across the two cold profiles cost `$0.05616` at frozen ClassB prices/1,000 repeats. This credits no callback, socket, SQL or byte-price saving. The previous approximate denominator is conditional; neither a measured R17 score nor an all-profile score exists here. The proven net lower bound remains **zero** until admission, callback and response-cache behavior are measured. Warm profiles have no ordinary GET opportunity and should not acquire extra startup warming.

The recorded maximum response cache bytes plus one chunk plus R17's256KiB reservation leave11,923,510 bytes at16MiB and3,699,547 bytes at64MiB before other metadata/ownership. This is promising boundary arithmetic, not peak/owning proof. A large certificate, ring capacity, socket registration change or a new response can still force eviction. A protected policy is not an always-hit guarantee, and a synthetic low-budget or many-file scan must expose eviction/thrash productively.

## Required source and API plan

1. Own one private immutable protected slot in the Native budget domain, indexed by exact digest and length, with a one-entry charge. Prefer NativeHost ownership so the same code can synchronously trim response entries/certificates and the slot before every admission. Putting it behind HttpServices' interior mutex without a coordinating budget hook would admit bytes before Native can trim its cache and is insufficient.
2. Admit only the first **already-required, successfully hash/length verified GET** when empty and budget-eligible, <=16MiB. Never admit from PUT/import bytes, a dummy probe, fixture key, generation number, branch, prewarm or observer/setup operation. State the generic production admission phase policy; startup/stats cannot silently warm timed reads. After pressure eviction the next ordinary eligible GET may refill it. Pushes invalidate responses but may retain this immutable slot. Register/reset/recovery/failure paths re-evaluate the same budget rather than blanket-preserving uncharged bytes.
3. Add a snapshot resolver for exact ordered selected descriptors that merges the protected verified chunk, a prefetched verified **missing** batch, and remaining ordinary verified reads. The existing `(prefix_count, VerifiedChunks)` assumes `chunks[..count]`; it cannot express an arbitrary first missing batch. Do not disguise missing items as a prefix, reorder pack chunks or return payloads under different descriptors. Validate every selected pack/ref/chunk descriptor before the first read and verify all selected bytes before native files are written.
4. Native planning computes the complete Full/projection selection independently of cache, then its first missing bounded batch. Authenticate both the stable full selection and exact cache/missing descriptor sets through the existing generation/manifest/request/nonce/session binding. Gateway/DO still check fresh authoritative generation and the published manifest fingerprint before missing GETs. Native re-derives the cache-aware plan before consuming the paid batch. If a cache eviction or local-generation change invalidates it, discard/charge the verified response and take an explicit ordinary safe fallback before receiving mutation. No hidden retry of attempted mutations.
5. **Separate stable receive validation from ephemeral cache state.** Current `receive_prepared` calls `gateway_plan` again after prefetched Full materialization and compares `plan.selection`. Cache admission during that materialization changes a cache-aware plan and can invalidate its own valid token. Validate ephemeral missing/cache bindings before admission/materialization, then mint the one-use Full receive token only after exact reconstruction. At transaction entry validate current authority, exact stable Full snapshot/request identity, session and scope; do not depend on a newly computed post-admission cache mask. Keep ordinary receive's unconditional reconstruction policy and shared request_error_cleanup, dirty recovery, quarantine/thin-pack validation, GC, publication and retirement unchanged.
6. Prefer the existing productive all-cache fallback initially: Native returns unsupported before object I/O; gateway performs its ordinary fresh authority check; Native reconstructs Full/projection solely from authenticated selected cached bytes. No synthetic GET. A true empty-batch authority continuation would need a separately versioned explicit capability, strict bounded proof/framing and independent gates; current nonempty batch validation must not silently weaken. All-cache means zero real payload reads, not stale authority.

## Ownership and budgets that must be proven

A cached Arc must own only the intended verified chunk, or charge the **entire** retained backing allocation. An8MiB slice into an18MiB bulk frame costs18MiB if that frame remains owned. An immutable `Arc<Vec<u8>>` can move an already verified Vec without copying payload, but its charge includes Vec capacity, allocation/Arc headers, key storage and slot state. An `Arc<[u8]>` conversion may copy; measure it. Neither length alone nor strong_count alone establishes owning size.

Request-local Arc clones are allowed to share the same immutable allocation while reconstructing one request. They are not separate payload allocations, but cannot justify an uncharged cross-request owner. If pressure evicts the slot while the current request holds a clone, classify that lifetime honestly as request-local and retain peak allocation evidence. Do not turn the request clone into a global pending cache or plan map. Current `VerifiedChunks` owns `Vec<Vec<u8>>`; preserve its exact descriptor proof boundary or introduce a narrow private immutable verified handle, never a public unchecked-byte shortcut. Native file reconstruction still needs correct owned concatenation; avoid copying a16MiB cached chunk twice just to satisfy legacy Vec APIs.

Count protected payload capacity+slot metadata in the same32MiB budget as response entries, ring capacities, advertisement/history certificates, pending publication metadata and R17 socket reservation; count the slot against256 entries. Every reserve/trim/insert/register/reset/error path must evict or reject admission before exceeding the bound. Protection is a preference, not an exemption: if required metadata or responses cannot fit, evict the slot and charge any later GETs. Verify exact maximum retained ownership, not only the metrics sampled after fetch.

Cached bytes do not skip manifest validation, generation authority, ordered selected membership, declared lengths or immutable digest validation. A cache hit must come from an unforgeable immutable verified handle or re-verification. Corrupt local cache state fails safely or is invalidated before selecting a fresh charged path; it never masks malformed metadata. Noncached R2 failure/receipt ambiguity remains a failure with actual work retained. Surviving pushes or a physical deletion is safe only because use still requires current selected manifest membership and exact immutable digest/length; the cache is not a storage durability witness.

## Callback and cold-view semantics

Removing one GET does not automatically remove a DO call. Re-batch the actual first **missing** descriptors under unchanged16MiB/256-entry limits, keep remaining reads ordered, and price actual gateway authority, service DO calls and R17 Upgrade rotations. All-cache fallback may alter planning/Native CPU and round trips even when it removes no DO call. Reduced response-cache capacity may cause expensive read-response misses. No callback gain or non-regression is credited before matched traces.

The cold codec view remains disposable and is reconstructed on ordinary misses; only this bounded immutable payload slot persists. After a successful push the newly published chunk/output has never been GET-admitted, so normal latest fetch should naturally perform positive actual R2 GETs. Demonstrate that property on the actual workload. Do not force an unconditional/dummy GET, use a benchmark no-cache flag, prewarm outside timers, relax the cold latest guard or relabel receipts to satisfy it. Historical/push reads may legitimately be all-cache.

## First actual production-workerd gate

Use a clean Rust-driven candidate/control with exact frozen source/common Worker and no prewarming. A real first fetch admits from its counted GET, then small/large/thin push, immediate retirement-window latest/history fetch and projected-to-Full receive prove exact Git bytes/refs/fsck/archive and reduced actual GET/read bytes. Record one protected admission, subsequent hits, pressure eviction/refill and actual socket/DO/Worker accounting. Include a real full16MiB cached allocation and maximum response overlap; latest after each push retains naturally positive GETs.

Exercise a deliberately all-cache read through ordinary fresh authority fallback, cache eviction between plan/continuation, external generation change, local cache corruption, failed noncached GET, lost continuation and ambiguous receiving publication/retirement. Verify no cached stale selection, duplicate logical receive or recovery bypass. Show the Full receive token survives its own legitimate cache admission. Admission from PUT/setup/stats must remain zero. A longer immutable scan plus certificate/entry/socket pressure proves productive eviction instead of assuming protected survival or ordinary LRU hits. Native restart loses the slot and resumes ordinary verified GETs.

These are correctness/topology/ownership gates, not a performance result. Independent exact-source review and a fresh four-profile screen must establish net cost and all latency/resource guards afterward. The separate schema/early-owner idea is outside R18 scope. Full bill stays NULL; no acceptance, global floor, plateau or stopping decision follows. **STOP READY.**

## Exact inputs and source identity

- `/tmp/git-zero-post-r17-next-design-audit-data.json` SHA256 `5bf4940996dbba23a87747621c3c6a512da02c596d30b91d3c365099a74807a9`.
- `/tmp/git-zero-post-r17-next-design-audit.md` SHA256 `79def940b3daae7dbceda8d0188559e8b9b100dd0bd8d4c2d7f63f992cdc9a1e`.
- Supporting independent JSON: `/tmp/git-zero-protected-chunk-independent-design-review.json` SHA256 `377c05fbdc31bd9c90733337d05f5db0de55308f781dada65be94d40290c0ee0`.
- Inspected clean commit: `c128197cc2cf1761e93832c63eec4c25c1268249`; this review does not approve R17 source.

| Source under `/tmp/git-zero-hosting-socket-rpc` | SHA256 | Bytes |
| --- | --- | ---: |
| `examples/repository-host/src/native.rs` | `b8e160ad0494156279be5ab5e6c7640e642d8da55abedeeba07852703f99ec4b` | 113829 |
| `examples/repository-host/src/native/history.rs` | `b30f3432e3d6b09d872fd0d10271c90b9398d75a335bb1a1775a1c7449e66d93` | 21101 |
| `examples/repository-host/src/lib.rs` | `c16b17fa37165273d4e8ea739a898b89efebbf54f407e8a902e379dbd0650b61` | 49237 |
| `examples/repository-host/src/storage.rs` | `31cb3e8bdba18ee7f48f951164763e0d1c536192f685ca21179cd8166a36d8c0` | 50248 |
| `examples/repository-host/src/cloudflare.rs` | `3ba886c9827e5e2d5f064c899245bb19f00b03c114c63c5568086b6a3f4a34ab` | 109205 |
| `examples/repository-host/src/socket_rpc.rs` | `64d6c5b4732772bd1e14017e55103a2307e59ef2d543f7e82406579847019ef6` | 18106 |
| `examples/repository-host/src/first_read.rs` | `502d6513633fa1845f6c97c1883914c21aaa08347adb25d7bb9dd71581280947` | 25758 |
| `examples/repository-host/cloudflare/src/first-read.js` | `3535717a794e0c0b0fcf3f4777271124ce51fc74b47db7d094843396e06374ac` | 11900 |
| `examples/repository-host/cloudflare/src/index.js` | `0b3183bb89d7920c9f1d0868895ec13414d74e74e096f8181b30654e418c0bbd` | 91566 |

Written at 2026-10-18T07:28:13.783082+00:00.
