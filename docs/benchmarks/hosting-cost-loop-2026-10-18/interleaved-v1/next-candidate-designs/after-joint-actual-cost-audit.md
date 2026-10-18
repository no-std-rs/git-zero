# Actual R20 next-cost design audit

**No new candidate is ready for implementation.** Existing bounded grouping has **$0 further known request-cost opportunity on these receipts** while retaining the same GET bytes, authority/masks and 16 MiB application payload bound. Straightforward coalescing of hot objects fails the write-byte guard. Larger physical containers with logical range reads remain a **separate conditional format/service-protocol design**, with a $0.018 gross hypothesis and no proved net gain. This audit grants no implementation, benchmark, promotion or publication GO.

Read/offline work only. Outputs are this Markdown, `AUDIT.json` and its self-contained Python arithmetic verifier. No production edit, build, server/runtime, Git-object generation, push, CI, deployment or delegation; no live process/environment inspection. All production source claims use immutable Git blobs. Existing fit and bootstrap conclusions are context, not new approvals or additive savings.

## Frozen inputs and independently reconstructed cost

- R20 source `b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0`, exact R19 parent `f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`.
- Frozen primary commit `d5cdefe53fc24f34114292b7d19563e73afea293`; fit source `5b523c9c9b8d8b8876281371c888d5af77757df2`, exact R20 parent. The common six-module Worker tree is `7f41db4407963ed04e6cbe19556b1416633caf18` at all four pins.
- Actual eight original reports and three score/comparison files from fresh run `39128927970/1`, plus score provenance, root independent guard/service-scope reviews, the prior residual/fit/bootstrap designs and preliminary fit review. JSON pins every input by byte length/SHA-256, plus 17 R20 source blobs by Git blob/SHA-256 and equality to the specified frozen primary blob.

I independently summed every scheduled original integer receipt and used frozen `cost.rs` (verified **2026-10-17**), not inferred invocation ratios: **R19 $1.292455436180 → R20 $1.212471436180**, a **6.1885306%** known-cost drop. Both priced samples agree. The objective repeats **each** of four profiles 1,000 times, with exactly 32 pushes/56 fetches each; actual retained R2 peak is priced once for 30 days. **1% R20 = $0.0121247143618.** Worker CPU, DO duration, monthly SQLite storage and Native compute remain unknown; full bill is **NULL**.

The verifier checks all 2,112 R19/R20 scheduled rows including separately retained warmups, 25,344 fixed-phase field closures, actual API counts and all 16 priced trial costs. Its initial overstrict assertion that the DO instance never changes failed; the corrected verifier preserves actual epoch changes and checks complete authoritative Worker receipt totals across them. No source or receipt was changed. Instance-local key displays can reset and expose only top ten keys even when `truncated:false`; they are not a complete ordered GET timeline.

## Grouping is already present and its opportunity is exhausted on these rows

R20 collects **all selected missing descriptors across packs** before calling the adapter (`src/lib.rs:348–390`). The adapter already forms the largest bounded prefix of at most 16 MiB (`src/cloudflare.rs:319–349, 983–1022`). The gateway's first authority request incorporates the same first missing batch (`src/native.rs:2012–2030`; Worker `index.js:1448–1466`). Worker `readBulk` performs individually metered, ordered R2 GETs inside that single DO invocation (`index.js:1388–1435`). Parallelizing those SDK calls does not remove a DO dispatch or an object GET.

For a scheduled operation, let **B** be actual GET bytes, **P** successful gateway first batches and **C** standalone read callbacks. With unchanged authority/write work and payload bounds, even freely repacking these bytes needs:

`C ≥ max(0, ceil(B / 16,777,216) − P)`.

Current **C equals this optimistic lower bound for every one of 704 priced R20 rows**, and all 352 separate warmup rows. Restricting ordering or exact chunk packing can only strengthen the bound. I independently cross-check C using actual DO counts, two actual `manifest_get`s per operation, actual PUT counts, and completed socket RPC deltas. No failed/discarded first-read or socket fallback is hidden in these rows.

| Profile, each priced sample | GETs | Actual GET bytes | First gateway batches | Standalone read callbacks | Volume minimum |
| --- | ---: | ---: | ---: | ---: | ---: |
| 16 MiB cold | 85 | 219,786,498 | 39 | 6 | 6 |
| 16 MiB warm | 0 | 0 | 0 | 0 | 0 |
| 64 MiB cold | 229 | 1,849,065,488 | 72 | 75 | 75 |
| 64 MiB warm | 0 | 0 | 0 | 0 | 0 |

Across all four profiles, **917 DO requests = 704 fresh authorities + 132 write RPCs + 81 standalone read callbacks**. The 314 GETs must not be priced as 314 separately removable callbacks. Worker upgrades/rotations remain actual independently counted requests. Smaller RFC frames are transport pieces, not DO invocations (`service-socket.js:84–116`; `socket_rpc.rs:134–197`). Multiple bounded responses bundled into one larger application dispatch, authority reuse, changed masks, or retries would require a different reviewed protocol rather than this regrouping change. Keep socket reservation 262,144 B, pool 32, cap 32, joint cache 32 MiB and response/receive bounds unchanged.

**Cheap falsifier:** find one matching actual row with C above this lower bound before proposing code. None exists here. There is no saved callback to fund additional allocation, capability negotiation, calls or ambiguous fallback. This is a narrow receipt-based rejection, not a global request floor.

## Physical-layout checks and limits

For all **256 priced R20 pushes**, current PUTs already equal `ceil(actual bytes written / 16 MiB)`: 32/32 total PUTs in the 16 MiB profiles, 34/34 in the 64 MiB profiles. Existing staging already packs the compressed pack and index together (`src/lib.rs:118–158`). The first 64 MiB snapshot is **34,207,816 B**, partitioned **16,777,216 / 16,777,216 / 653,384 B**. This proves no simple within-push boundary coalescing of the current bytes saves PUTs under the existing 16 MiB new-blob limit. It proves no universal write/CAS floor or compression bound.

Combining an already published hot secondary with a later one requires rewriting at least **4,372,216 B / 4,391,027 B**. One such copy alone adds **10.91% / 6.62%** to fixed workload write bytes (40,066,586 / 66,312,587 B), exceeding the 1% allowance before mapping, read amplification or peak storage. Reject that straightforward concatenation proposal. Small-pack-only combinations have no established ≥1% net case; charge every copy, subset read, retirement, control allocation and lost cache hit before considering them.

The 64 MiB sample-0 second anchor chunk is read **80 times**. A conditional boundary-shift calculation holds those GET multiplicities/current hit identities fixed, moves δ bytes from that chunk to the tail and grants ideal batch packing. Even the maximal permitted tail gives only **53 callbacks / $0.00795**; generously deleting **all seven** actual 64-cold Worker service requests raises this to only **$0.01005**, below 1%. This is **not** a bound over changed residency: first-fit can admit a shortened second chunk, reject the enlarged tail and displace hot secondaries. Root-repartition feedback therefore remains unresolved; no layout-wide rejection or global floor follows. Only sample 0 has the continuous complete hot-key counter needed for this arithmetic; sample 1's DO instance changes are preserved rather than reconstructed away.

## Separate conditional format hypothesis: larger physical object, logical range chunks

If each initial 64 MiB snapshot could be stored as **one** 34,207,816 B physical object while retaining exact logical 16 MiB chunk digests/masks and serving exact ranges, two PUTs could disappear in each cold/warm profile. Conditional Class-A ceiling:

**4 × 1,000 × $4.50 / 1,000,000 = $0.018**, **1.48457%** of current known R20 cost. That leaves just **$0.0058752856382** above the 1% target before **all** added known costs. No GET-byte, Class-B, CPU or duration saving is credited. Two added Class-A operations across the four-profile repetition would already exceed that headroom; the gross saving is conditional, not measured or guaranteed.

**It is not supported now.** `Chunk` contains only content-addressed `key` and `len` (`storage.rs:196–201`), verified against the complete returned payload (`storage.rs:247–258`). `readBulk` uses full-object `BLOBS.get(key)`, with no range (`index.js:1407–1421`). Standalone PUT, pending tail, exact upload fields and socket application messages all enforce the current 16 MiB new-payload limit (`storage.rs:110–129`; `index.js:14, 212–235, 1479–1485`). Smaller transport fragments do not make a 34 MiB application upload admissible.

A separate design would need authenticated physical key/total-size/offset/logical-length/digest mapping, exact range verification, private logical-chunk allocations rather than retaining whole containers, revised authoritative fingerprint/legacy decoding, and physical-key liveness/catalog/pre-delete-peak accounting. Shared slices cannot authorize deleting a still-live container. Mapping/control ownership belongs inside existing budgets. Partial upload, stale/lost committed CAS, orphan recovery, sync-before-retirement, same-Native cleanup, restart and ambiguity must remain correct and honestly charged.

**First falsification requires no runtime:** specify an exact durable upload message and physical-operation sequence for 34,207,816 B under the unchanged complete 16 MiB receive limit. Enumerate every actual billable create/part/complete/abort/PUT, callback, SQL row and recovery action; do not count a new multipart/streaming path as one free PUT. If it needs at least the current three Class-A operations per initial 64 MiB push, the four-PUT hypothesis is false. If only one Class-A per profile can be justified, price mapping/range/legacy/retirement costs against the remaining margin and prove each profile's read/write/live/peak/client/HTTP transfer amplification stays ≤1%. Only then would separate independent format review and a Rust first-push/range-read/fault/restart gate be warranted. There is no concrete admissible upload sequence or net lower bound here.

## Handoff

Keep the existing fit-response source pending its own allocator/fault/restart/warm/HTTP review, and deferred bootstrap separate with actual extra startup GC reads priced against the selected measured predecessor. No additional ≥1% optimization is promoted here. The larger-container hypothesis is a conditional future design seam, not a reason to edit the current source or change limits.

The fresh R20 screen remains **Inconclusive**, plateau-ineligible, with five p95 groups failing; five fresh original-R1/final pairs per profile remain undone. No winner, speed, full-bill, optimum, plateau, exhaustion or stopping claim follows. Exact source refs, all inputs, row proofs, conditional arithmetic and unknowns are in [AUDIT.json](/tmp/git-zero-after-joint-actual-cost-audit/AUDIT.json); [verify.py](/tmp/git-zero-after-joint-actual-cost-audit/verify.py) reproduces the offline arithmetic. **STOP after handoff.**
