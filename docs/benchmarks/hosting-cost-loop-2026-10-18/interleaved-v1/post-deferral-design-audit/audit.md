# Post-deferral design limits audit

**Finding:** I found no further concrete ≥1% known-cost candidate within the retained Native Git, complete-pack prefix projection, immutable bundled-chunk, 16 MiB bulk-read and per-PUT/catalog-sync/CAS interfaces. After the corrected prefix, bounded catalog pages and normal retirement deferral actually succeed, a conditional stopping bound is available. Those changes are not yet jointly measured, so this report establishes neither acceptance nor a present plateau. It does not establish global optimality or prove that every possible with-have pack-selection algorithm is exhausted.

This bounded independent audit read frozen source at `7e7856a3f6c279a8f97956a4475cb12f847b1f7e`, existing reports and local Git 2.53.0 source. It changed no repository source and ran no build, test, service, probe, CI, push or benchmark. Only this report and its compact JSON calculation artifact were written under `/tmp`. Python performed offline JSON arithmetic only; no execution runner or MJS was introduced.

## Actual completed evidence

The newly completed four-profile, two-sample R8–R11 archive is `/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r8-r11-screen/run-39092442019`. Independent final guard review remains the parent's task. I reconstructed every R11 pack/chunk sequence from successful PUT bytes, compaction and retirement receipts and exactly matched all **704 measured R11 workload rows**, including every GET count/read byte and ordered final manifest. This separately confirms the earlier 704-row R8 reconstruction.

The R11 fixed objective is **$1.94098593618** per stated 1,000-repetition/four-profile objective. It is the exact sample mean of the fixed push/fetch receipts plus peak R2 retention held for 30 days, using the existing dated `cost.rs` prices. The mean replay has 132 PUTs, 128 winning publications, 84 successful retirements, 1,148 Class B operations, 1,292 Worker requests, 1,292 DO requests, 348 SQL written rows and 1,174.5 SQL read rows. Actual fixed R2 bytes are 5,826,008,336 read and 212,758,346 written. SQLite is 24,576 bytes in every sample.

| Actual R11 priced component | USD |
| --- | ---: |
| R2 Class A | 0.594 |
| R2 Class B | 0.41328 |
| Worker requests | 0.3876 |
| DO requests | 0.1938 |
| SQL written rows | 0.348 |
| SQL read rows | 0.0011745 |
| Peak R2 retention | 0.00313143618 |

All cold R11 samples record **zero history projections**. The two-token capability correction at `/tmp/git-zero-hosting-history-git-caps` therefore remains a separate unmeasured change. Its normal prefix opportunity is still 69 GETs and 9 callbacks in 16 MiB cold, and 69 GETs and 6 callbacks in 64 MiB cold, per replay. That is **$0.05643 conditional fixed saving**; it is not an achieved R11 saving. Warm profiles have no read saving. The required extra final full restore remains separately charged in complete physical scope; the earlier $0.01224 projection is not added to or subtracted from the fixed objective here.

Observer traffic is retained separately. The actual mean stats-only scope has 1,060 LIST/Class A operations, 1,060 DO requests, 1,064 Worker requests and 1,091 SQL read rows, with zero writes/deletes/GET bytes: **$5.249291** of known request/row charges under the same repetition scaling, excluded from the fixed objective. Complete observed physical totals, also including startup, are 1,196 Class A, 1,148 Class B, 2,364 Worker and 2,360 DO requests, 372 written and 2,293.5 read SQL rows. These counters are not production savings opportunities and observer inventory must never repair or certify production state. CPU, DO duration, monthly SQLite retention and Native compute remain UNKNOWN; full bill remains NULL.

## Conditional floor and the remaining small envelope

The following conditions define the retained model, rather than a universal Git-hosting lower bound:

1. Keep the exact workload/client HTTP schedule and authoritative per-public-request generation check: 704 gateway Worker/DO requests. Keep the external Native codec topology and existing authentication/authority contract.
2. Retain immutable bundled pack/index chunks, current pack/compaction output and selection sets, 16 MiB physical chunk and bulk payload caps, at most 256 bulk entries, full validation before materialization and no cross-cold-operation object-byte cache. Cold receive uses Full; classified historical reads use the certified prefix; unsupported requests use Full.
3. Each of 132 physical successful PUTs persists catalog/peak information and syncs before its publishing CAS. Each of 128 winning publications has a subsequent durable CAS write/sync. Deferred retirement can share a following PUT's row write but cannot collapse these two separated barriers. Thus **260 normal SQL written rows** remain. This is not a lower bound for a new staging/persistence API.
4. Preserve strict native receive/quarantine, exact refs, full historical fsck/archive, truthful failure receipts, durable deletion/GC/peak safeguards, mixed/legacy/restart handling and all original memory/storage/latency guards.

The exact unordered bin-packing calculation was repeated against all 704 new R11 rows. It finds **zero additional callbacks saved** for either complete snapshots or corrected-prefix snapshots. Its search exhaustively assigns the at-most-13 chunk sizes to capacity-16-MiB bins and respects the already nonbinding 256-entry cap. Reordering alone is exhausted for these read sets; it is not a claim about different read sets or layouts.

With corrected prefix, pages and deferral assumed successful on their normal paths, the replay has 1,010 Class B reads and 1,277 Worker/DO requests: 704 gateway + 132 publication/upload service calls + 441 minimal bulk-read service calls. The normal SQL floor is 260. Four actual R11 ALTER rows plus four projected page ALTER rows give the illustrative 268-row total; any real extra migration, routing, reconciliation, split, proof or fault work must be measured and added.

Starting from the actual R11 score gives the explicitly conditional reference:

`1.94098593618 − 0.05643 − 0.056 + 0.004 − 0.028 = $1.80455593618`.

Even granting the impossible generosity of deleting **every** remaining SQL read, **all** R2 retention and **all eight** assumed DDL rows, the removable envelope at those reference counts is only:

`0.0011745 + 0.00313143618 + 0.008 = $0.01230593618 = 0.6819371%`.

The remaining structural components cost at least **$1.79225** under the listed interfaces: `.594 Class A + .3636 Class B + .3831 Worker + .19155 DO + .260 normal SQL writes`. This is a deliberately optimistic mathematical lower bound, **not an achievable zero-retention/zero-read operating design**. Its useful actual stopping test is: if a freshly measured accepted final candidate costs **less than $1.81035353535** and still has those structural counts/interfaces, eliminating every nonstructural known charge cannot reach 1%. The conditional reference leaves only about **$0.00579759917** for additional overhead before this particular bound must be recomputed. Extra costs from page routing or proof recovery are not assumed away. Final per-profile regression guards also still apply; an aggregate bound does not waive them.

## Why a narrower historical or seeded read is not established here

`native.rs:660–720` materializes complete selected `.pack` and `.idx` files only after `lib.rs:242–288` validates the descriptor graph and verifies all selected chunks. The existing `BlobStore`/`blobs/read` interface returns complete content-addressed chunk bytes; indexes share those bundles. There is no independently addressed index, object range, demand-paged file, object-membership summary or closure graph. Reading indexes first through this interface pays for the same chunks; membership alone cannot establish transitive tree/commit and delta-base closure. Git's `packfile.c:608–672,815–844` expects regular `.pack` files, correct size/header/trailer and matching `.idx`; absent packs, fake sparse zero data or index-only stubs are not a supported lazy-read protocol.

The private certificate (`native/history.rs`) has per-pack earliest introduction floors and per-ref publication stamps, not an object graph or certified have cut. Compaction propagates the minimum of every input floor. For the measured append chain, a no-have historical root needs all its ancestor commits; the final observer inventory has exactly 32 unique/encoded commits, so there is no duplicate commit witness allowing an arbitrary required complete pack to disappear. This supports the existing prefix construction. It does not independently establish exact minimality of every intermediate selection.

Seeded clients require particular care. Git 2.53.0 `upload-pack.c:477–523` parses each have and its parents; `:285–360` sends wants and haves as positive/negative revision roots to native `pack-objects --revs --thin`. `revision.c:1125–1210` and `list-objects.c:283–337` traverse excluded commits and tree edges. A client claiming possession is not a certificate authorizing the server to omit arbitrary old packs or thin-delta bases. **Conversely, `revision.c:1150–1156` tolerates missing parents on an UNINTERESTING traversal. Therefore it would be wrong to claim this source proves native Git must physically read every old ancestry pack for every with-have request.** `upload-pack.c:1214–1233` also marks ref OIDs without parsing each object's payload. Neither shortcut establishes a safe suffix-only repository by itself.

A possible future suffix/edge design must prove the exact wants-minus-haves closure, required boundary trees and thin bases, all relevant multiple-root/alias/merge cases, and native behavior without quietly relying on missing-object tolerance. It must preserve complete authoritative history while constructing its temporary view, bind evidence to actual successful strict receive/compaction, fit the joint 32 MiB/256-entry and 32 KiB proof budgets without retained object payload maps, provide a supported regular-file/range or complete-pack reconstruction path, and price actual extra index/graph/range calls and fallback. No such bounded design with a demonstrated ≥1% net opportunity was derived in this audit. It remains outside the conditional stopping model, not a globally rejected optimization.

For large pushes, `native.rs:984–1040` deliberately performs a Full cold reconstruction and uses its objects as quarantine alternates, preserving all previous refs. Git `receive-pack.c:2317–2377` performs actual strict index-pack/fix-thin validation; `:1991–2068` runs connectivity checks before object migration. `connected.c:102–126` uses actual `rev-list --objects --stdin --not --all --alternate-refs`. Skipping old packs because the new tip is recent does not preserve that retained interface or prove arbitrary incoming thin-base availability. A separately certified receive closure or lazy object provider would be a new design; Full is an explicit retained contract, not a proof that every byte is a theoretical Git necessity.

## Disposition and remaining gates

Complete independent review of the measured R11 guards, then measure the actual capability correction, pages and any implemented deferral on committed/pushed clean source using the authorized independent cloud-VM workerd process. Retain the two-pair screen and mandatory **five fresh original-R1/final pairs across all four profiles**. Reconstruct actual final traces again, verify pending debt is empty at the real endpoint, include actual schema/routing/recovery costs, peak retention, unchanged caps, physical observer scope and UNKNOWN billed dimensions. If actual final counts depart from this model, recompute the bound rather than substituting this projection for receipts.

This audit supports stopping additional **known-cost micro-optimization inside the named retained model** once those changes and final controls succeed and the actual bound holds. It does not accept an unmeasured candidate, manufacture two rejected measured candidates, resolve latency failures, certify a full bill, or declare a global optimum. Review complete. STOP.

## Provenance

Compact arithmetic, per-sample receipts/group counts, final commit inventory and SHA256 report identities: `/tmp/git-zero-post-deferral-limits-audit-data.json` (SHA256 `e50f38ff0852a6e3c2a0b217519018e226938d385ce97b7499b98d0b84a1f54a`). R11 score SHA256: `f973c4e7c35fd3a85461b32201c6e25075367a195d944967d4212e573c742f27`.

The shared primary was clean at the initial frozen HEAD. During this audit the parent advanced it to `d334330d130f87de3c78028040323d7afaa3c430` and added review/archive documentation. A final read-only comparison to the frozen Git objects confirmed identical Native, bridge, repository and Worker source bytes; only the history classifier had acquired the separately reviewed capability correction. The frozen certificate hash below is verified directly against the frozen Git object, and no conclusion treats that correction as measured.

Read source SHA256 anchors: Native `2d4a4ab13b152ad2be2a02d0648685678765134831958207e4b7445f4169faa3`; history certificate `d37f9721f90eab0919c349bcd285dd9acfa42ba25834e3c105475d29c87d7e44`; Worker `f5f72b40ec605814be6a6a0311df44e255ffdc3b402afec2e66454aa5711a811`; Git upload-pack `4d59ef09a579b72ae06314dbc7b9d421604a52de0ea478f790a42c4d7250138d`; Git revision `f83e21c73b055e2865ef6997c035b83fc4904541d37acfee6a47e7f874d44f3e`; Git packfile `6a2018c2c6e56de819a26e3912fdbba5089cc17c90caed86b5259a31d9e28470`.
