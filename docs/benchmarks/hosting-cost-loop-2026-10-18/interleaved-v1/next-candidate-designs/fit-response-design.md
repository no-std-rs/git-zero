# Residual cost after conditional Joint: bounded design audit

**A credible next policy is non-destructive admission of newly built upload POST responses.** At the default conditional pressure model it saves another **70 GETs / $0.0252 per 1,000 four-profile repetitions** after Joint, with fewer read bytes in both cold profiles. This is a proposed experiment, not measured R20/R21 savings or implementation approval. At the 4 MiB pressure stress it loses three GETs. **No positive unconditional net saving is proved; net saving can be negative.**

Read/offline work only. Only this file and its JSON companion were written. No source, build, tests, server/runtime, benchmark, CI, push, deployment or delegation; no cancelled shadow-server work and no inspection of mutable author runtime or other owned processes. I read immutable R19 Git blobs and archived fresh reports. Conditional Joint means exact R19 with only the secondary-headroom heuristic removed; I did not inspect or approve mutable R20 source.

## Exact narrow rule

Apply only to **cold Cloudflare Joint's upload POST response retention**. Keep the ordinary produced public Git response and current 16 MiB eligibility. Before any retained clone, ring reservation or eviction, preflight every source retention stage using actual owning values:

`current cache_bytes + current metadata_bytes + candidate upper_weight <= 33,554,432`

If the response ring must grow, additionally include the existing source's **full old-plus-new ring bound**, `(max(current_capacity,4)*2)*size_of((String,Response))`, on top of current metadata, which already charges the old ring. Also require `response_count + chunk_count + 1 <= 256`. Candidate body, key capacities, Arc/header ownership and subsequent exact cloned weight remain charged exactly as current source; no new reservation or fake size is introduced.

**When at least one verified chunk is resident and either preflight cannot fit without eviction, return the already generated response without retaining it.** Do not alter any current chunk, previous response or ring on this rejection. When it fits, follow the existing exact-charge insert path; a retained clone or growth must never unexpectedly evict a chunk that the preflight promised to preserve. Same-source controls are ordinary Joint response priority versus Joint plus this one fit-POST rule.

Keep discovery, no-chunk, non-cold and default paths unchanged. Required certificate/control/socket/pending-publication ownership still has priority and uses the existing newest-secondary-through-anchor, then response eviction order. Keep first-fit verified GET admission, no live replacement for admission, 32 chunks inside 256 combined entries, actual 32 MiB joint ownership, 16 MiB response/chunk eligibility, confirmed authoritative Full prune, promotion and all authority/storage/protocol behavior. A productive >14 MiB through exact16 MiB response remains eligible whenever it fits; this introduces no lower body cap or permanent chunk protection.

The production source supports the hypothesis: `native.rs:1754–1785` lets successful response retention call `trim_cache`, while `trim_joint:1942–1957` evicts chunks first. Thus retaining a one-off response can remove several reusable chunks. This policy changes only whether optional POST retention causes that pressure. It may lose useful response hits on other workloads, which the gate must measure.

## Independent fresh-R19 reconstruction and conditional replay

I independently reconstructed all **704 actual priced rows** from fresh run `39118509165/1`: four profiles × two samples ×88 rows. Inputs are actual upload lengths, fixture/schedule/ref names, source append/compaction order and trusted historical selection. For current R19 Multi I reproduce **every row's GET attempts, read bytes, chunk hits, owning bytes, resident count, admissions, confirmed prunes and pressure evictions**, plus all256 push inventories and final ordered pack lengths. Current cold totals are153/354 GETs,569,108,040/2,449,962,286 read bytes; warm GETs/admissions remain zero. No old receipt counts were substituted.

Each actual response boundary also reconciles source advertisement pkt-line growth from the real ref names, recorded output bytes and response-owning growth. Discovery and POST owning overheads are1055/1128 bytes in these frozen rows. The first eligible discovery is inserted between the two initial reconstructions. Current pool charges use399 bytes per handle,8 bytes per slot and256 bytes per nonempty backing; the fresh every-row ownership match corroborates this source/allocator model for these inputs. Old/new pool growth overlap, retained nonempty capacity, Full pruning and all response entries are included.

The replay uses actual response owning sizes and entries and source-derived ordered selections. It neither imports the author's replay nor substitutes fictitious object sizes. The archived top-key maps are **not** a complete ordered GET/control/RPC timeline. Recorded boundaries do not reveal in-request control/response-ring/header clone peaks, socket rotation, pending publication or held-owner pressure. The analytical 0.5/1/2/4 MiB stress is **not** retained memory or a proposed reservation. Future code must charge actual values and separately check ring growth; the replay does not execute that complete within-row sequence.

At1 MiB stress, each sample independently gives:

| Profile | Conditional Joint GETs | Fit-POST GETs | Joint read bytes | Fit-POST read bytes | Skipped POST entries |
| --- | ---: | ---: | ---: | ---: | ---: |
|16 MiB cold|85|53|219,786,498|131,883,773|9|
|64 MiB cold|229|191|1,849,065,488|1,734,213,513|10|
|Warm, both sizes|0|0|0|0|0|

Both cold fetch-only read-byte totals also fall:131,883,641→88,161,298 and1,113,126,653→1,056,042,479. No actual latest-recheck response hit is lost in this conditional replay: naturally skipped large latest responses occur at different requests from those repeated rechecks. There are no GET/read-byte regression rows at1 MiB. These facts describe this reconstruction; **no resident-set superset or general response-hit guarantee follows**.

The first16 MiB skip is `history_with_haves_16`: retaining12,811,240 response-owning bytes would pressure21,779,161 chunk-owning bytes,7 owners, plus4,210 existing response bytes and control. The first64 MiB skip is `latest_fetch_13`:4,270,119 response bytes against30,606,193 chunk-owning bytes,5 owners, plus2,329 existing response bytes. Later required operations can reuse residents retained by skipping these entries. These are conditional source/evidence examples, not observed new cache events.

| Analytical nonchunk/within-row stress | Further GETs avoided | Further read bytes avoided | ClassB-only dollars |
| --- | ---: | ---: | ---: |
|0.5 /1 /2 MiB, each|70|202,754,700|$0.02520|
|4 MiB|-3|386,805,531|**-$0.00108**|

At4 MiB the16 MiB profile saves38 GETs, but64 MiB adds41; lower bytes do not rescue the GET-cost loss. Preserve this falsifier in review. Required metadata pressure or changed masks/batches/authority/Upgrade paths may also erase the gain. No Worker/DO/SQL/storage/read-byte dollar credit is used.

Frozen `cost.rs`, verified-date2026-10-17, charges70×1,000×$0.36/1,000,000=$0.0252. That is1.9498% of actual provisional R19 known cost**$1.29245743618**; its1% threshold is$0.0129245743618, leaving only$0.0122754256382 before added known costs at that reference threshold. The actual preceding Joint denominator and net improvement remain unmeasured. Full bill isNULL; WorkerCPU, DOduration, monthlySQLite and Native compute remain unknown.

## Bounded rejected alternatives and cross-generation check

I also replayed oldest-secondary, oldest-all and largest-chunk-first real-pressure orders using these actual owning lengths. At1 MiB, oldest-secondary saves only18 additional GETs/$0.00648, below1%R19, and16 MiB reads rise1.78%. Oldest-all saves75GET/$0.027, but cold read bytes rise9.28%/14.24%; largest-first saves87GET/$0.03132 while reads rise39.11%/14.56%. They fail unchanged read-byte guards **against conditional Joint** in this model. These checks actively reject these narrow alternatives; they do not bound every possible eviction/admission policy.

Cross-generation immutable-response reuse is not currently supported: `cache_key:1711` hashes the generation, confirmed receive clears the response ring at1500, and recovery clears it at806. Dispatch still validates fresh generation authority before lookup. The fixed workload has only four logical wanted/have repeats per profile after excluding already served same-generation rechecks. Both64 MiB no-have repeats refer to >16 MiB bodies and cannot be retained under the unchanged cap. Across four profiles, only12 logical repeat rows remain size-eligible; actual R19's eligible rows incur just**3 GETs** and**1 DO callback above the two required fresh gateway authorities** per repetition. Even generously removing every SQL read in those rows, the direct credit ceiling is**$0.001254**, about0.0970%R19. Gateway Worker requests and fresh authority remain charged.

That ceiling is conditional on unchanged other work and only direct repeated-response elimination. It is **not** a whole-system retention/feedback bound or proof that exact wire requests repeat. Raw rows contain wanted/have identifiers, not full request-body, capability/nontransport-header or physical-framing hashes. A true reuse design would have to bind exact supported requests/capabilities and thin bases, and revalidate every requested ref/OID against fresh advertised/current active authority after deletion/force push. Prepared masks/tokens cannot cross generations. A lease, weaker freshness, new billing schema, or authority bypass would be out of scope. This direct opportunity is too small; stop exploring it here.

No PUT/CAS floor follows from current32/34 ClassA counts or current code shape. Larger physical objects are not automatically forbidden by the16 MiB cache-entry limit, but require a separate actual layout/read-amplification/peak/mutation/fault proof under the128 MiB repository and all unchanged caps/schema. Moving Git bytes into new SQLite storage/schema, delayed durable pushes or changed authority/storage assumptions cannot be silently credited. Protocol-preserving authority batching, safe layout/dedup and other within-budget priority rules remain untested credible alternatives. Larger cache/response caps, extra entries, prewarming, workload changes and shifted observer work are rejected objective changes, not savings.

## Cheap first proof and progression limits

After independent design review, exact Joint source approval and explicit root ordering/GO, compare **same-source Joint versus Joint+fit-POST** through real production Native/workerd with an unchanged Worker/config and a Rust runner. Use fresh Native/cache/client roots and identical sealed durable contents; separately charge startup GETs/authority and prove no startup/observer/PUT/warm admission. This audit grants no code or runtime GO.

First demonstrate one genuine productive response that would evict verified owners under Joint, optional retention skipped under the candidate, and subsequent required ordinary read/receive reusing those exact owners. A16 MiB history/next-receive sequence and64 MiB replacement/response-pressure sequence should suffice before broader replay. Include naturally repeated latest rechecks to charge lost response hits; do not require the modeled70GET result or use fixture keys in production.

Require byte-identical public Git bodies/refs/manifests and strict index-pack/fsck/archive; true external-base thin receives, compaction/retirement and following reads; productive >14 through exact16 MiB and >16 MiB uncacheable responses; 32/256 limits; real parent/payload/key/header/Arc/pool/ring capacities and old/new overlap; actual socket/certificate/control/pending-publication peaks. Preflight skip must not clone/reserve/mutate retained owners. Mandatory nonresponse pressure must still reclaim them normally. Record all attempts and failures.

Repeat held authenticated-mask eviction and paid-discard/ordinary fallback, stable Full receive tokens across legitimate admission/prune, exact descriptor/current order/membership/hash/length, corrupt/noncached suffix, projection/observer no-prune, confirmed prune/promotion/refill, stale/lost committed CAS, partial retirement/no ambiguous replay, same-Native cleanup before the next PUT, restart, warm and HTTP paths. Measure every actual R2/Worker/DO/SQL/Upgrade/rotation/receipt phase; directly prove PUT/DELETE/LIST/CAS and written/live/peak objects/bytes/SQLite outcomes. Missing CPU/duration stays unknown.

Any correctness/cap/write/storage/resource/receipt/latency failure or insufficient conservative1% known-cost margin blocks progression. A successful cheap gate still requires independent exact-source/ownership/fault/accounting and ledger review, then a fresh matching all-four-profile screen. R19's screen remains formally inconclusive and plateau-ineligible despite passing36resource/4profile-cost guards;3p95 limits fail. The five mandatory fresh originalR1/final pairs across all four profiles remain undone. No speed, winner, floor, optimum, plateau or stopping claim follows.

## Sealed evidence

R19 immutable source `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`; Native134,028B / SHA256`a0a70c78b98a814474bf060f76dabee6e0acbd3c0cd0cc3d1d25758353ce0d1b`. Fresh R19 score239,409B /`9bcc9e7ac17546ca1b7a72ee83f5ce0ee2de8cf06b402c09ec06238652664bb4`. The JSON pins all13 inspected source blobs, all four fresh R19 reports, both score/comparison files, prior design/prep and root-review artifacts. It contains the self-contained independent verification program, exact current-row validation digests, every policy/stress aggregate, regressions, skip evidence and cross-generation bounds.

Companion `/tmp/git-zero-after-joint-residual-audit.json`: **98,722B**, SHA256 **`51c9502c4d41c169d1a9c0f42f55e8dfab91d63235fb48a2bc7a9b9a83d9958f`**. Sealed UTC`2026-10-18T11:15:04.950661+00:00`. Bounded design returned; **STOP** for fresh independent review.
