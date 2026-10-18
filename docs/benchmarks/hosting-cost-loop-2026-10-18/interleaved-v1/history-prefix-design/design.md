# Next cost design: ephemeral historical prefix lineage

Design-only review, 2026-10-18. Recommend a clean implementation **after the pending screen has been reviewed**, subject to the gates below. This is not an implementation approval, measurement, acceptance, speed claim, or plateau claim.

The proposed optimization keeps bounded in-memory provenance and uses it to materialize a safe historical prefix of current packs for narrowly classified cold protocol-v0 fetches. It adds **zero durable manifest bytes, SQLite rows, R2 objects, or object-payload caches**. Recovery or an external publication loses selectivity and returns to conservative full reads. This removes the unresolved SQLite page risk in the earlier durable-sidecar design.

Evidence is the complete R5 reports from `run-39084173778`, with the current **16 MiB physical chunks**, two measured samples per profile, 32 pushes and 56 fetches per sample. Offline reconstruction matches the GET count and read bytes in **all 704 measured workload rows**, each sample's fixed counters, and the ordered final pack-size sequence. This is arithmetic over existing files, not a new benchmark. The exact report hashes, every row, every reconstructed generation, all costs and scopes are in `/tmp/git-zero-next-cost-projection.json`; `/tmp/git-zero-next-cost-analysis.py` reproduces the primary calculation.

The initial source reference was `b2df9560c7461fee85dc6433c2fdf97437b196fb`. The parent concurrently integrated R9 as `8af8c48fb272624bb67a0cb3923fad154c3a6dcf`; its native publication diff was also read. Later parent documentation appeared at `457e2fa`. This task made no repository changes, builds, tests, benchmarks, network requests, installs, or service/infra probes. The only new files are reports and offline analysis under `/tmp`.

## Decision and exact financial scope

Using only the repository's dated 2026-10-17 linear rates and 1,000 repetitions of each profile, plus each observed R2 peak held once for 30 days:

| Scope | R5 known USD | Conditional candidate USD | Saving | Reduction |
| --- | ---: | ---: | ---: | ---: |
| Declared fixed objective: exactly 32 pushes + 56 fetches/profile | 2.33324193618 | 2.27681193618 | 0.05643000000 | 2.4185233% |
| Existing whole non-stats traffic, **plus the candidate's required final full restore explicitly charged** | 2.37886993618 | 2.33467993618 | 0.04419000000 | 1.8576047% |
| All observed physical traffic, including every observer request and R2 inventory LIST, plus required restore | 7.62816393618 | 7.58397393618 | 0.04419000000 | 0.5793006% |

The fixed projection meets the protocol's >=1% objective gate. The all-observer physical scope does **not** improve by 1%; it must never be described as doing so. The second row is an explicitly defined additional operational comparison, not the scorer's existing whole-trace result: existing scoring excludes stats, whereas this comparison charges the newly required stats restore. No whole-trace counters have been substituted into the fixed objective.

Fixed savings comprise 138 fewer R2 GETs ($0.04968) and 15 fewer native-to-Worker/DO read callbacks ($0.00675). The mandatory final restores add 24 GETs and eight callbacks, costing $0.01224; complete physical savings therefore become 114 GETs and seven callbacks, or $0.04419. One callback saves one Worker request and one DO request at this R4+ gateway topology; R2 requests remain charged separately. R2 read bytes themselves have no egress charge in this evaluator.

Full cloud bills remain **null**. Worker CPU, DO active duration, monthly SQLite retention, and native Git compute are unmeasured. No wall-clock time is converted to any of them. The all-observer values above are known linear operation scenarios, not invoices or newly verified provider prices.

The earlier optional durable JSON sidecar adds 150 bytes at generation 32. Actual compact R5 final manifests are 4,728 and 4,923 bytes, and current SQLite is 24,576 bytes. Those numbers do not prove another 150 bytes fit existing SQLite pages; one extra 4,096-byte page would be a 16.7% regression. The selected ephemeral design writes no such field and performs no migration, avoiding that issue rather than claiming an unobserved same-page result.

## Provenance certificate and correctness invariant

Use a private, non-serialized certificate bound to the exact authoritative generation, current pack sequence and current refs. Its binding includes pack identities, representation/split lengths, ordered chunk keys/lengths, and exact ref name/value pairs in bytewise UTF-8 order. Retirement debt can be excluded deliberately: clearing confirmed deletion outcomes in the existing in-memory manifest does not change the live object universe. A same-generation full rehydrate of this exact live snapshot preserves the certificate. It is not authorized by an arbitrary supplied JSON field or by pack name, order, size, Git timestamps, or inferred ancestry.

The certificate consists of fixed identity/version fields and two arrays aligned to the current authoritative manifest: a conservative earliest-introduction floor for each pack, and a witness stamp for each current ref name/value pair. It retains no object IDs, duplicated ref strings, old snapshots, downloaded chunks, reachability bitmap, native pack files, or per-object index. OIDs used for request matching come from the already retained authoritative manifest.

Bootstrap an existing fully reconstructed snapshot at generation N by assigning **floor 0 to every existing pack** and stamp N to every current ref value. A known-empty generation-0 snapshot has empty arrays. Never reconstruct older floors from physical ordering. This conservative adoption cannot omit any pre-bootstrap objects. After a restart, dirty recovery, ambiguous publication, external generation, changed publication policy, malformed certificate binding, or unsupported object-preserving operation, discard the certificate and bootstrap this way only after an ordinary complete reconstruction. A metadata-unaware writer requires no compatibility format; its external generation simply invalidates local lineage.

For a strict confirmed publication N+1:

- A newly validated incoming self-contained pack gets floor N+1. Untouched packs retain their floors.
- A compaction output gets the **minimum floor of every selected input**, including new incoming inputs and inputs whose entries are all duplicates. The floor is not the output creation generation or the maximum input floor.
- An unchanged exact current ref name/value retains its witness. Added or changed values get N+1 even when the receive sends no new pack, points to existing objects, changes an alias, or returns to a previously deleted value. Deleted refs lose their stamps.
- Carry pack floors through `CandidatePack` and the actual selected compaction plan, then align them with the exact `StoredPack` output order. Compute ref stamps against the same next refs used by CAS. Do not independently guess floors after publication.
- Take the old certificate out before a transaction that can publish. Install the candidate certificate only after strict validation, durable publication success, complete exact retirement receipt validation, and the native full-view/ref update succeed. A stale CAS, upload failure, failed/ambiguous callback, lost reply, or local post-publication failure drops it. R9's staged final chunk is still just an incoming representation with floor N+1; metadata cannot certify it before the staged upload/CAS result is confirmed.

Invariant: every object present at an earlier observed generation G has a current representation in a pack with floor <=G. Existing adoption objects satisfy it because their floors are zero. Append preserves older witnesses. Compaction preserves the input object union and transfers the lowest floor even if it chooses another duplicate representation. Current thin-push completion is self-contained, and compaction checks delta-base closure; selected complete packs therefore retain their decoding dependencies. If pruning, cross-pack thin representations, representation-dropping deduplication, reachability GC or another replacement policy is added, invalidate this version until a new proof exists.

For each wanted **and each have** OID, require an exact decoded match to a current authoritative ref value with a valid stamp. With multiple aliases, using the maximum matching stamp is a simple conservative rule. Let G be the maximum stamp over all commands. Read every current pack with floor <=G in original manifest order. This contains complete closures for wants and haves; newer haves expand the prefix. Unknown commands or OIDs select Full **before any storage read**. A rejected client receive may conservatively discard provenance even when its manifest did not change; this reduces savings, not correctness.

## Retained memory, request eligibility and safe APIs

Enable only for cold Cloudflare views. Warm mode keeps its existing full native view and allocates no lineage certificate. Cap optional lineage allocations, for example at 32 KiB; exceeding that cap disables selection and uses Full. This is an eligibility cap inside the existing cache budget, not another 32 KiB reserve.

**All retained allocations share the same existing 32 MiB total:** response entries, their keys/headers/overhead, R8 advertisement template/certificate, lineage struct/array capacities, and any simultaneously retained candidate metadata. Count capacity and fixed owning structures, not only array lengths. Cache entry count remains <=256 and each response body <=16 MiB. Evict through the existing response policy if allocation requires it; expose the resulting evictions and misses. A pending certificate cannot hide its allocation in an uncharged side map. Bounded request-local classifier lookup storage is discarded before the next request and must not become a retained OID cache.

At generation 32 the workload has 11 packs and 33 refs: the two u64 arrays contain 44 elements, **352 payload bytes**, plus explicitly accounted struct/allocation overhead. Existing measured response-cache occupancy at raw operation boundaries peaks below 12,822,000 bytes; this gives substantial workload headroom, but is not a proof about allocator peaks or future transfer/cache behavior. Preserve `cache_bytes` as response-only if compatibility requires it, and separately publish lineage bytes and honest total retained bytes including the advertisement. Do not claim the old response-only field enforces the joint budget.

The classifier is an opaque Rust planner, not a new user-visible protocol. Require the exact identity-encoded upload-pack POST MIME/path, protocol v0, branch-only current refs, the fixed reviewed Git/config/environment policy, at least one want, and complete bounded pkt-line consumption of ordinary wants/flush/haves/terminal flush-or-done. A small independent classifier limit (e.g. 128 KiB body and 2,048 OID commands) falls back to existing Full handling. Existing 64 MiB request limits and all normal parser errors remain in force.

Reject the optimized route for v2, gzip/other encoding, tags/replace/notes namespaces, include-tag, shallow/deepen/filter, URI/want-ref, unknown capabilities or ambiguous/truncated/trailing grammar. Review an explicit capability allowlist; never infer eligibility from substrings or benchmark operation names. Require the R8 fixed branch policy/type certificate where useful; unsupported legacy ref kinds remain supported through Full. Do not synthesize refs or enable arbitrary-SHA1 wants.

Suggested private boundaries:

1. `validate_snapshot(&Manifest) -> ValidatedSnapshot`: perform **whole-manifest** ref/name/count, pack uniqueness, representation, total <=128 MiB, key syntax, repeated-key length agreement and chunk framing/split validation before planning or I/O. Invalid omitted-pack metadata still fails. Reuse `snapshot_chunks` validation, split from payload reading.
2. `plan_v0_fetch(request, validated_snapshot, private_lineage) -> Full | Historical`: return a bounded immutable selection bound to that exact snapshot. No caller-supplied arrays can manufacture a trusted lineage proof.
3. `read_snapshot_files(validated_snapshot, selection)`: flatten the selected packs in original order, consume existing `get_many_verified`/`VerifiedChunks::into_data` against **exact keys, lengths and order**, then reconstruct pack/index splits. Keep legacy separate pack/index and oversized scalar paths; floor-zero legacy packs remain included. Do not reorder/reuse repeated GETs or materialize any native file before all selected chunks verify.
4. `rebuild_view(validated_snapshot, selection)`: remove the previous disposable view first, create exactly the selected pack/index files and eligible original refs, then commit scope. There is one view, with no hidden full-view directory, alternate full cache, or retained downloaded-chunk map.

Projected native refs must be a subset of **original current** entries whose stamps are <=G. Every requested OID remains represented by at least one original ref; preserve those exact names/values. Do not leave newer dangling refs pointing into omitted packs. The fixed HEAD file can remain a dangling symref for this POST-only view; it grants no extra authority. Native Git still handles negotiation, ACKs, connectivity traversal and wire encoding.

All consumed chunk hashes, framing, native push fsck/connectivity and compaction integrity checks stay strict. Selected reads do not authenticate omitted unrelated pack payloads: the existing full-fetch behavior incidentally sweeps their corruption too. Whole-manifest structural validation remains complete. If full-repository payload authentication on **every** historical fetch is an explicit external contract, this candidate must be disabled rather than representing selective authentication as equivalent to that stronger contract.

## View lifecycle, recovery, cache and concurrency

Track `Invalid`, `Full {generation}`, and `Historical {generation, cutoff, snapshot_binding}` separately. A valid partial view is **not dirty**. Ordinary `/metrics` remains lightweight and never restores Full merely to fill a field. Expose scope, selected/total pack counts/bytes, cutoff and explicit full-restoration counters. Invalid materialization or ambiguous publication uses existing authoritative recovery, invalidates both certificates and response cache as appropriate, and cannot serve another request from a partial failed view.

Cold misses always read their selected bytes from actual counted R2, even if the identical partial view is already on disk. This is selection, not a new cold-cache exemption. An ordinary same-generation full read replaces the view and preserves valid lineage. It must not reset all floors to zero just because the view was rebuilt. Every actual request retains the existing authoritative generation/witness check; external generation invalidates plans and response cache.

Response-cache keys keep the full current authoritative generation and original request method/target/headers/body. A valid cached response can be served while the native view remains partial, leaving scope unchanged. Do not key on historical cutoff alone. R8's eligible metadata-only advertisement can continue using its authoritative manifest certificate; an actual native advertisement fallback must establish Full. Preserve its complete metadata checks and do not relearn a certificate from a partial native advertisement.

Receive preparation must establish Full exactly once before the quarantine alternate is used, including large pushes and ref-only pushes. Unsupported native RPCs, generic object-reading paths and global object inventory also require Full before use. Avoid two full rehydrates when the normal cold path already performs one. `/_object-stats` calls `objects::inventory` over every authoritative pack and must establish Full from Historical. That restoration is explicitly counted in stats/whole traffic. Recovery and GC always use the full manifest, never projected refs or pack selection, and selected reads perform no authoritative writes or retirement.

On a selected read's corruption, ordering, length, framing or native-invariant failure, leave scope Invalid and return the error. Never silently retry Full after partial I/O; that hides errors and adds unplanned traffic. Eligibility fallback occurs before reads.

The existing host serializes requests and one writer; prepared responses own bytes. Keep that contract through selection/publication/retirement, and exclude external publication changes through the existing generation CAS/witness protocol. This is not a new multiprocess or production streaming-reader guarantee. Production leases and authorization remain separate existing design requirements.

## Actual per-row historical projection

The following service bytes/GETs/bulk calls are identical in measured samples 0 and 1; wire response sizes are preserved separately for **each** raw row in the JSON and deliberately have no invented projected value. All non-historical rows and both warm profiles are unchanged by this candidate. The rows presume actual complete pkt-line eligibility and preserved request/cache behavior; the original HTTP bodies were not archived.

### 16mib-cold, each of samples 0 and 1

| Operation | Cutoff | Actual R2 bytes | Selected bytes | GETs actual → selected | Bulk calls actual → selected |
| --- | ---: | ---: | ---: | ---: | ---: |
| history_no_haves_04 | 3 | 8,659,221 | 8,621,049 | 4 → 3 | 1 → 1 |
| history_with_haves_04 | 2 | 8,659,221 | 8,582,885 | 4 → 2 | 1 → 1 |
| history_no_haves_08 | 1 | 13,031,504 | 8,544,710 | 5 → 1 | 1 → 1 |
| history_with_haves_08 | 5 | 13,031,504 | 12,916,995 | 5 → 2 | 1 → 1 |
| history_no_haves_12 | 1 | 17,403,789 | 8,544,710 | 6 → 1 | 2 → 1 |
| history_with_haves_12 | 4 | 17,403,789 | 12,916,995 | 6 → 2 | 2 → 1 |
| history_no_haves_16 | 9 | 21,776,056 | 17,289,278 | 7 → 3 | 2 → 2 |
| history_with_haves_16 | 14 | 21,776,056 | 21,699,717 | 7 → 5 | 2 → 2 |
| history_no_haves_20 | 2 | 26,148,281 | 12,916,995 | 8 → 2 | 2 → 1 |
| history_with_haves_20 | 7 | 26,148,281 | 17,289,278 | 8 → 3 | 2 → 2 |
| history_no_haves_24 | 14 | 30,520,502 | 26,033,764 | 9 → 5 | 3 → 2 |
| history_with_haves_24 | 16 | 30,520,502 | 26,033,764 | 9 → 5 | 3 → 2 |
| history_no_haves_28 | 15 | 34,892,739 | 26,033,764 | 10 → 5 | 3 → 2 |
| history_with_haves_28 | 25 | 34,892,739 | 34,778,232 | 10 → 7 | 3 → 3 |
| history_no_haves_32 | 4 | 39,265,016 | 12,916,995 | 11 → 2 | 3 → 1 |
| history_with_haves_32 | 6 | 39,265,016 | 17,289,278 | 11 → 3 | 3 → 2 |

### 64mib-cold, each of samples 0 and 1

| Operation | Cutoff | Actual R2 bytes | Selected bytes | GETs actual → selected | Bulk calls actual → selected |
| --- | ---: | ---: | ---: | ---: | ---: |
| history_no_haves_04 | 3 | 34,378,764 | 34,321,787 | 6 → 5 | 3 → 3 |
| history_with_haves_04 | 2 | 34,378,764 | 34,264,807 | 6 → 4 | 3 → 3 |
| history_no_haves_08 | 1 | 38,769,839 | 34,207,816 | 7 → 3 | 3 → 3 |
| history_with_haves_08 | 5 | 38,769,839 | 38,598,925 | 7 → 4 | 3 → 3 |
| history_no_haves_12 | 1 | 43,160,937 | 34,207,816 | 8 → 3 | 3 → 3 |
| history_with_haves_12 | 4 | 43,160,937 | 38,598,925 | 8 → 4 | 3 → 3 |
| history_no_haves_16 | 9 | 47,552,014 | 42,990,013 | 9 → 5 | 3 → 3 |
| history_with_haves_16 | 14 | 47,552,014 | 47,438,070 | 9 → 7 | 3 → 3 |
| history_no_haves_20 | 2 | 51,943,058 | 38,598,925 | 10 → 4 | 4 → 3 |
| history_with_haves_20 | 7 | 51,943,058 | 42,990,013 | 10 → 5 | 4 → 3 |
| history_no_haves_24 | 14 | 56,334,049 | 51,772,129 | 11 → 7 | 4 → 4 |
| history_with_haves_24 | 16 | 56,334,049 | 51,772,129 | 11 → 7 | 4 → 4 |
| history_no_haves_28 | 15 | 60,725,109 | 51,772,129 | 12 → 7 | 4 → 4 |
| history_with_haves_28 | 25 | 60,725,109 | 60,554,207 | 12 → 9 | 4 → 4 |
| history_no_haves_32 | 4 | 65,116,174 | 38,598,925 | 13 → 4 | 5 → 3 |
| history_with_haves_32 | 6 | 65,116,174 | 42,990,013 | 13 → 5 | 5 → 3 |

Every historical row has one discovery-cache hit, one POST miss and one rehydrate. No discovery saving is included in this candidate. Crucially, final `latest_recheck_32` has a response-cache hit, zero rehydrates and zero R2 traffic in both samples of both cold profiles. It leaves the preceding partial history view in place. The final full global inventory therefore adds:

| Profile, each sample | Additional GETs | Additional R2 bytes | Additional Worker + DO callbacks | Fixed GET/byte/callback saving | Complete physical net GET/byte/callback saving |
| --- | ---: | ---: | ---: | --- | --- |
| 16mib-cold | 11 | 39,265,016 | 3 | 69 / 110,985,807 / 9 | 58 / 71,720,791 / 6 |
| 64mib-cold | 13 | 65,116,174 | 5 | 69 / 112,283,259 / 6 | 56 / 47,167,085 / 1 |

No extra R2 PUT, delete, written byte, live/peak byte, manifest update or SQL row mutation is introduced. Bulk reads perform no SQL query in the normal already-initialized DO path; real constructor/eviction row counts remain actual measured observations, not deterministic CPU or lifetime predictions. Cache/response changes and runtime eviction effects still require receipts from the next measurement.

The reconstruction uses each actual push's written bytes and observed compaction event. The initial pack remains; at generations 5,9,...,29 the three preceding small packs plus the new incoming pack compact once into a ~4.37/4.39 MiB output, floor generation-minus-three. Four such outputs exceed the unchanged 8 MiB input budget and never merge at level one. Native packet/index lengths, not Git dates or branch labels, establish the current physical layout; the fixture's current ref witnesses determine the query cutoff. The current final pack floor sequence is `[1,2,6,10,14,18,22,26,30,31,32]` (initial floor 0 is equally conservative after a generation-1 bootstrap). This reconstruction reproduces every actual full read before selection is applied.

## Independent alternative audits and limits

**Tighter read binpacking/order:** an exact bounded offline binpacking calculation, using every reconstructed whole snapshot and the unchanged 16 MiB payload / 256-key cap, finds the existing ordered greedy read batches already achieve the unordered minimum for every one of the 704 rows. There are zero callbacks to save, including after removing R8's projected discovery reads. Typical full chunks occupy a bin alone; ~4.37 MiB compaction outputs fit at most three per remaining bin. Arbitrary reordering cannot reduce the current workload's priced requests without changing chunks/layout/row semantics, and risks changing failure read order. This is a substantiated bound for this layout and trace, not a global architecture lower bound.

**Compaction retirement:** each trace retires 21 actual old keys, 84 across four profiles. R6 can fuse the callbacks, but every required R2 delete and catalog row mutation remains real. R2 deletes have no priced unit in this model. Keeping retired bytes or skipping durable debt is not an optimization. With the present catalog representation, deleting each present key still changes one row even if a SQL statement deletes several keys. A carefully reviewed `DELETE ... RETURNING size` could remove the preceding catalog SELECT while preserving retry/absence semantics; at most 84 existing read rows would be avoided, approximately **$0.000084 per 1,000 replays**, far below 1%. This is only a narrow floor under the present catalog contract, not proof that all control-plane redesigns are exhausted.

**Removing the per-chunk catalog in favor of R2 HEAD plus a durable aggregate:** actual fixed SQL writes reveal 14 high-water updates per 16 MiB trace and 16 per 64 MiB trace, not one per PUT. Across four profiles there are 132 PUT catalog writes + 84 deletion writes + 60 high-water writes = 276 metadata row writes, excluding the 128 manifest publications. A durable replacement live-count/live-byte/peak row must change on all 132 PUTs and 84 deletes, or 216 writes, saving only **60** writes ($0.060), while HEAD existence/size checks add 216 Class B operations ($0.07776). That simple redesign therefore **increases known cost by $0.01776** before small read-row effects. Even eliminating all remaining measured fixed SQL reads would save only about $0.0012; it cannot reverse this regression. Omitting aggregate durability would require restart inventory/reconciliation and can introduce R2 LISTs during reads, violate no-Class-A read guards, or lose exact peak/live accounting. More advanced schemes would need an independent idempotency, crash/orphan, migration, concurrency and receipt design; they are not established savings here.

**More precise selected-pack closure:** current indexes are bundled with pack bytes and have no cold object/range/graph interface. Finding membership by reading indexes first adds requests; membership alone does not prove transitive or delta closure. For the immutable compaction outputs in this fixture, the final inventory reports 32 encoded/unique commits and no cross-pack duplicate commit witness. At generation 32 the selected initial pack and compaction outputs each contain a required ancestor commit, so an exact complete-pack closure cannot simply omit those files for the recorded no-have request. This is a bounded conclusion for those retained files and that request, not proof that every transient earlier small pack or every with-have case is optimal. With-haves graph boundary algorithms or finer R2 ranges could have different opportunities, but require a new bounded codec/index design and measured priced-call evidence. No unproved object map, retained full history, or altered storage cap is smuggled into this design.

**Native `pack.allowPackReuse=multi` alone:** local pristine Git 2.53.0 source confirms the bitmap prerequisite. `Documentation/config/pack.adoc:30` describes multi-pack reuse through an available multi-pack reachability bitmap; `builtin/pack-objects.c:4495` first calls `prepare_bitmap_walk`; `pack-bitmap.c:2130` opens a bitmap, and `pack-bitmap.c:2481` disables multi reuse unless the bitmap is a MIDX with the bitmapped-packs chunk. Native `NATIVE_CONFIG` enables bitmap use but native reconstruction creates only `.pack`/`.idx`, with no bitmap/MIDX/commit-graph upkeep. Setting the option alone has no demonstrated path to reduce this native workload's codec CPU. Producing/retaining such artifacts is a separate performance design with reconstruction, maintenance, memory/storage and read-write guard implications. Any CPU effect would be known-dollar neutral in this evaluator. It also cannot explain the retained warm 138-second tail localized by the parent to ~143 ms of native codec time and zero R2 reads; there is no causal timing claim here.

These audits rule out specific simple changes and support a conditional remaining-cost discussion **after** staged upload and prefix selection are measured. They do not establish two measured cost-flat candidates, formal plateau, a global cloud-cost optimum, or permission to stop while this viable prefix design remains unmeasured.

## Gates for a clean implementation and later decision

The pending R5–R8 screen and R9 implementation must be reviewed before choosing the clean preceding anchor and implementation boundary. Counter savings above are based on actual R5; R8/R9 remain projections until their own measurements. History discovery already hits cache, and prefix does not alter push upload/retirement, so there is no source-level overlap with their claimed savings. Joint metadata eviction and changed cache/transfer behavior still prevent blindly summing scores.

A clean implementation is warranted when it can meet all of these gates without changing the benchmark or caps:

1. Private provenance construction proves minimum propagation across multi-level compaction/duplicates, exact current ref-value stamp updates, and drop-on-failure/external-generation/restart behavior. No-pack refs, deletion/re-add, aliases, force-policy fallback and generation overflow need explicit handling.
2. Full pkt-line classification covers multiple wants/haves/negotiation rounds and positive legacy/v2/tag/shallow/filter/encoding fallback before I/O. Core malformed omitted-pack metadata still fails; only unsupported optimization metadata/state falls back.
3. Ordered `VerifiedChunks` integrity, legacy representations, delta-base completion and corruption failure behavior remain strict. Missing selected objects fail rather than retrying Full. The chosen interface does not promise an omitted-byte corruption sweep.
4. Partial-to-large-push/unsupported-native-RPC/discovery/global-inventory transitions restore Full once; metrics never do hidden restoration. Same-generation full rebuilds preserve lineage. Cache hits leave partial scope valid; external/ambiguous state drops lineage. Full authoritative manifests alone drive retirement.
5. Every retained metadata allocation is charged jointly within 32 MiB, every eviction is visible, and existing response count/body/request/ref/snapshot/compaction limits remain unchanged. Warm mode remains full. Original cold latest fetches still perform actual R2 reads.
6. A fresh unchanged matching interleaved screen uses actual Worker/DO/R2 receipts on all four profiles and records the final global full restore. Fixed known cost must improve >=1%, and R2 read/write/live/peak, SQLite pages, varied-fetch HTTP/client bytes, strict content validation and paired Git-normalized median/p95 guards must pass. Large push reads and random historical fetches remain present. A failed transfer/latency/storage guard rejects or leaves the candidate inconclusive regardless of the arithmetic above.

Two-pair screens cannot formally accept a speed or cost winner and do not themselves establish plateau. Final validation remains five fresh paired trials of the original scalar baseline and selected final candidate. No speed improvement is claimed by this report.
