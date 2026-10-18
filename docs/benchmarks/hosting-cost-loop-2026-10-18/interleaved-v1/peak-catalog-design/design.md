# Carry durable peak in the catalog PUT row

Design-only decision, 2026-10-18: **viable for a clean, bounded implementation after the preceding screen/anchor is selected**, with the explicit capability gate and acceptance conditions below. Correctness does not require restoring the 60 normal-workload high-water writes. This is a conditional source proof and existing-file arithmetic, not an implemented or measured winner, a combined R9/prefix score, or a plateau/global-optimum claim.

No repository source changes, Git actions, builds, tests, benchmarks, network calls, or infrastructure/service probes were made. Deliverables are this report, `/tmp/git-zero-peak-catalog-projection.json`, and its offline file calculator `/tmp/git-zero-peak-catalog-projection.py`. Source reviewed: the parent's reported `0cd459ccedaa52608bd0d81a8a6b4c4683e27712` tree, especially `examples/repository-host/cloudflare/src/index.js` (`RepositoryServices`, `retirementKeys`, authenticated service bridge/registration), `src/cloudflare.rs` (`HttpServices::register`, reclaim), `src/storage.rs` (live-key/debt rules), and the existing Rust upload/restart contracts. No external SQLite/Cloudflare behavior was independently verified here; actual Wrangler behavior is an implementation gate.

## Bounded financial case

Existing actual R5 reports in `run-39084173778` have two measured samples per profile. Independently attributing each actual push's SQL writes to successful PUTs, present-key DELETEs, manifest CAS and the remaining high-water writes yields, per four-profile workload:

| Actual fixed rows | 16 cold | 16 warm | 64 cold | 64 warm | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| PUT catalog rows | 32 | 32 | 34 | 34 | 132 |
| DELETE catalog rows | 21 | 21 | 21 | 21 | 84 |
| Manifest rows | 32 | 32 | 32 | 32 | 128 |
| High-water rows | 14 | 14 | 16 | 16 | 60 |
| All SQL written rows | 99 | 99 | 103 | 103 | 404 |

Both samples give these counts. The machine report includes every push attribution, temporary peak and final live value, plus source-file SHA256s. It retains the source scopes and does not multiply the two samples into eight profiles.

Carrying peak in the already-written 132 PUT rows conditionally removes 60 writes and leaves 344 normal fixed row writes. It adds no R2 HEAD, GET, PUT, LIST, DELETE, payload bytes, manifest bytes, Worker request, or DO callback in the normal path. Additional constructor/schema read rows, migration writes, fallback writes and actual SQLite pages are not assumed zero.

Using only the repository's existing dated 2026-10-17 rates, the **gross** saving for 1,000 repetitions of each profile is $0.060. Against the actual R5 known scopes this is 2.5715% of fixed $2.33324193618, 2.5222% of whole non-stats $2.37886993618, and 0.7866% of all physical/observer $7.62816393618. Thus the all-observer scope does not grossly reach 1%. These are projections before added costs, not accepted final prices. The later R9 and prefix denominator is unmeasured here and is deliberately absent.

For the four-profile replay, let W be additional actual SQL written rows (including fresh-namespace ALTER and fallback) and R additional actual SQL read rows. At 1,000 replays, the saving is `0.060 - 0.001*W - 0.000001*R`, before any operation/storage changes. The gross read-only break-even allowance is 60,000 additional read rows per replay; the fixed >=1% gate permits about 36,667.58 additional read rows when W=0. A schema migration charged inside the first measured push must be included there, never relabeled startup. This allows a small, measured migration cost without assuming its cursor count.

Worker CPU, DO active duration, monthly SQLite retention and native Git compute remain null. No complete cloud bill or wall-clock-to-CPU conversion is claimed. The earlier HEAD-plus-aggregate audit remains negative: this proposal retains the catalog and adds zero normal R2 HEADs, so that audit's $0.01776 regression does not apply.

## Per-provider activation and compatibility

The next evaluation uses one common FINAL Worker against distinct Native source trees. An always-on Worker-only optimization would affect all providers and cannot establish an R11-vs-R10 saving. Use this minimal additive negotiation:

1. Only the new Native adds `catalog_peak: 1` to its existing `HttpServices::register` JSON body. Keep the method signature and all existing upload/retirement capabilities. This creates a genuinely distinct Native source tree without introducing a benchmark-only switch or extra callback.
2. The authenticated Worker register route stores `catalogPeak: body.catalog_peak === 1` in the existing bounded per-repository registry entry. Optionally acknowledge `catalog_peak: {version: 1}` in the existing registration response; contracts should inspect the acknowledgment. Registration does not dispatch a DO request or migrate SQLite.
3. Before forwarding any authenticated service request, delete the caller-provided private capability header (prefer stripping all `x-git-zero-internal-*` headers there, as the public gateway already does). Set `x-git-zero-internal-catalog-peak: 1` **only** from that repository's current registry flag. The DO is reached through the Worker binding, and the public Git gateway cannot mint this header. An unauthenticated request fails before register/forwarding. The local token already authorizes repository service mutation; this gate is attribution/configuration, not a new security principal.
4. The constructor retains the exact legacy `CREATE TABLE IF NOT EXISTS blob_catalog (key TEXT PRIMARY KEY, size INTEGER NOT NULL) WITHOUT ROWID`. Fresh old-Native namespaces remain v1 and use the original PUT/high-water/delete behavior. Read-only schema discovery distinguishes v1/v2; it must be included in real SQL receipts for every provider.
5. A validated catalog mutation carrying the trusted capability may migrate v1 to v2. No GET, advertisement/manifest read, bulk/scalar blob read, metrics observer, invalid request, registration or stale generation-checked GC may perform migration. Plain manifest-only CAS needs no migration. Validate the complete request before DDL. The first eligible PUT can migrate before its R2 operation; fused retirement may migrate only after winning CAS and before deletion.
6. Once a repository is v2, its durable schema selects v2 accounting for every client. Registry loss, unregister, Worker/DO restart, or an old Native reconnect must never downgrade it to v1 or ignore its catalog peaks. A fresh old-Native namespace still stays v1. New Native against old Worker can simply run legacy behavior with no savings; acceptance must demonstrate negotiation and the measured row reduction.

An unregistered capability cannot silently migrate a v1 repository. After a Worker restart the lost registry is re-established by the ordinary existing register flow; v2 durability is already recoverable without that registry. The persistent schema is repository ownership, not the transient current client flag. No capability belongs in R2, a manifest, Git objects or a public header.

Rolling back **Native clients** while retaining the new Worker is supported. Rolling back the Worker to old code after v2 has carried a peak is not automatically safe: old code ignores `peak_bytes`, and its GC reset could erase the last witness while its `high_water` lags. Document this migration boundary. Supporting old-Worker rollback would require an explicit durable high-water checkpoint before rollback; do not add routine writes to pretend it is free.

## Schema, state and invariant

For v2, migrate once on a mutation with:

```sql
ALTER TABLE blob_catalog ADD COLUMN peak_bytes INTEGER NOT NULL DEFAULT 0;
```

Keep the existing `high_water` table and row. Do not rewrite/backfill all legacy catalog rows, add an index, drop/recreate tables, or seed an extra row. Legacy high-water already protects the prior known peak; default-zero old catalog records gain witnesses only on actual later PUTs or existing GC reinsertion. `ALTER` and its physical/database-size cost require actual measurement.

Schema discovery can use `PRAGMA table_info(blob_catalog)` through the same counting `exec`. It is read-only. Never catch-and-ignore a failing speculative SELECT as a surrogate for metering schema discovery. A modern inventory query reads `COUNT(*)`, `COALESCE(SUM(size),0)` and `COALESCE(MAX(peak_bytes),0)` together. Read the existing `high_water` row separately. Do not rely on a bare `key` column being paired with `MAX(peak_bytes)`; select any owner in a separate explicit query only if needed. Legacy queries must not reference the missing column.

Track observed `peakBytes`, catalog live count/bytes, the last known durable fallback value, and an optional owner key plus its witnessed peak. The owner is one bounded key hint, not a durable truth or cache of object payloads. Constructor/reload computes peak as the maximum of catalog SUM, catalog MAX(peak_bytes), legacy high-water and any higher same-instance already observed floor. A constructor can leave owner unknown; the next confirmed PUT establishes it without a lookup.

Invariant: every previously durably known peak remains represented either by the legacy fallback row or by at least one durable catalog row. Destructive work must not erase the last such representation. Normal successful PUTs also persist a true pre-CAS, pre-retirement temporary peak. A key need not itself be the largest blob or a live manifest key: the field is the repository's historical aggregate peak, carried by an existing catalog record.

This invariant does not invent a stronger cross-service transaction. The existing successful-R2-PUT-before-catalog crash gap remains. A physical PUT followed by process death before SQLite has recorded it cannot be reconstructed from SQLite alone. Existing generation-checked authoritative R2 GC is still required. What is newly mandatory is observing and durably preserving that inventory's pre-deletion peak before GC erases orphan evidence.

## PUT ordering and failures

For a v2 PUT, under the existing serialized DO service gate:

1. Validate key/body and optional migration. Read prior catalog size (include peak if useful) using the existing key SELECT. No R2 HEAD is added. Same immutable key retries have zero live-object/byte delta.
2. Perform the actual counted R2 PUT. On an R2 error, do not claim successful upload/catalog/CAS or an increase merely because a buffer was allocated. A possibly ambiguous failed R2 operation still uses existing dirty recovery.
3. On returned success compute `nextLive = currentLive + newSize - previousSize`, and `nextPeak = max(observedPeak, recoveredDurablePeak, nextLive)` **before any CAS or retirement**. Preserve this observed peak in memory even if subsequent catalog work fails.
4. The one existing catalog mutation becomes `INSERT INTO blob_catalog(key,size,peak_bytes) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET size=excluded.size, peak_bytes=MAX(blob_catalog.peak_bytes,excluded.peak_bytes)`. No separate high-water write occurs. Compute integer bounds using existing safe-integer policy; do not truncate peak to object size or post-retirement live bytes.
5. Await the existing `storage.sync()` barrier. Only after success install the owner hint and commit the in-memory catalog count/byte changes as certain. Fused CAS follows this barrier exactly as today.

A catalog error means real R2 work may have happened but CAS must not start. A thrown upload sync means upload/catalog durability may be ambiguous; do not issue fallback PUT/CAS or delete the orphan. Invalidate owner/catalog certainty and reload actual SQL before relying on a witness again. A later destructive action must successfully sync a valid surviving witness/fallback before touching R2. Do not preserve an invalid owner merely because the failed operation planned to create it.

After divergence between R2 and catalog, SQL reload recovers durable peak witnesses but cannot certify exact physical live inventory. Continue the existing Native dirty-recovery/generation-checked GC path before claiming reconciliation. Do not claim the catalog is authoritative physical inventory, manufacture a complete receipt, or lower a same-instance known floor during recovery. The implementation must explicitly handle this distinction rather than using stale cached live totals after an exception. This is a focused failure-state gate, not a requirement to add ordinary-path R2 listing.

Stale CAS after a confirmed PUT leaves a durable orphan carrying its watermark; no retirement occurs. Ambiguous/lost CAS reply leaves the durable manifest/debt and uploaded peak unchanged; recovery reloads authoritative state and never blindly removes a possibly shared key. Peak accounting is independent of whether the uploaded pack won CAS.

## DELETE and partial retirement

Read the existing catalog key row before the real delete as today; its peak can share that SELECT. Before R2 DELETE, establish that the current observed peak survives deletion:

- A valid owner of sufficient peak at a different key, or a known sufficient durable fallback, proves survival without a new SQL write/query.
- If the owner is the key being removed, unknown or invalid, explicitly query a surviving catalog witness: `SELECT key,peak_bytes FROM blob_catalog WHERE key<>? AND peak_bytes>=? LIMIT 1`. Charge actual scanned/read rows; do not assume LIMIT bounds rows scanned.
- If none exists, upsert `high_water` monotonically to at least the known peak, then **successfully sync before the R2 DELETE**. A fallback error/ambiguous barrier prevents that key's deletion and returns the existing failure/debt outcome. Empty-repository deletion is covered by this rule.
- If prior metadata/barrier uncertainty exists, reload first and successfully establish durability; a visible SQL row is not an excuse to skip the needed barrier. Reads themselves do not persist recovery state.

Then perform actual R2 DELETE, the existing catalog DELETE, and existing live-total adjustment. Clear/update an owner only after actual successful mutation; any failed/ambiguous mutation invalidates it conservatively. Preserve existing HEAD-on-uncataloged-key behavior rather than adding HEADs to known keys. A failed R2 delete leaves the row/witness intact; a successful R2 delete followed by catalog failure leaves retryable divergence and the pre-delete peak still protected. Final retirement sync and outcome/debt validation remain unchanged. Failed fallback preservation marks that key undeleted; it cannot relabel a winning CAS as stale or fabricate deletion success.

Repeated absent-key deletion and immutable duplicate PUT must keep existing idempotent/absence semantics. Shared live keys are still filtered by `retirementKeys`/`Manifest::retirement_keys`; the peak feature does not infer reachability or remove any key omitted by those rules.

For the actual normal trace, every compaction performs one successful output PUT, then retires three older input keys. The output remains in the next live manifest and is excluded from retirement, so its newly synced catalog row is a valid surviving peak witness. Subsequent successful PUTs carry the same or greater peak. This proves zero normal retirement fallback writes without assuming high water equals final live bytes. For example, the actual 64 MiB final peak is 65,116,190 while final live is 65,116,174; the 16-byte difference must survive all restarts.

## Authoritative GC and restart

Before the first GC deletion or catalog erasure, use the existing generation-checked authoritative inventory to compute `H = max(all known in-memory peak, recovered catalog MAX peak, legacy high_water, SUM(size of ALL listed R2 objects before deletion))`. This includes orphan and retired physical objects, not only requested live keys. Check generation before all destructive work as today.

If the legacy fallback is below H, upsert it monotonically and successfully sync **before the first R2 DELETE**. If it already covers H, ensure no unresolved prior barrier remains. Only then perform removals. Before the existing transaction that clears/reinserts catalog rows, the same durable fallback must cover H. Reinsert surviving catalog rows with `peak_bytes=H` using the already-required INSERTs; update owner/counters only after successful reconciliation/barrier. Keep the fallback even if it is redundant. Empty GC needs no peak write when H=0. Interrupted/partial GC, failed reset, rollback or restart thus cannot erase the last historical witness.

A successful orphan PUT that raised peak and later loses CAS must remain counted even after recovery deletes it. A successful PUT whose catalog write failed is recovered by the pre-delete inventory SUM: persist H, delete/reconcile, restart again, and verify H remains. If an object had already disappeared externally before any catalog/inventory observation, its historical peak is unknowable under the existing system; this proposal does not claim otherwise. The model remains serialized service-controlled R2 mutation.

Metrics continues its existing observer LIST and computes actual live values without SQL writes. A higher metrics-observed peak can remain an in-memory floor until a subsequent mutation safely carries/preserves it; a pure metrics read does not become a write. A restart after a metrics-only observation of an untracked external/orphan object retains the same pre-existing observation limitation until GC/PUT persists it. Known durable peaks and pre-GC inventory observations are protected, which is the stronger destructive-operation requirement relevant here.

## Focused Rust verification and implementation gates

All new runners/contracts remain Rust; test-only JavaScript binding wrappers may extend the existing fault wrapper, with no JavaScript test runner or production fault switch. Do not change fixtures, operation counts, caps or scalar source history.

1. Real Worker SQL/accounting contract: negotiate only candidate capability; old fresh namespace remains v1 with original high-water writes. Header spoofing without registry flag cannot migrate; unauthenticated register fails. Confirm registration adds no DO call. A v1 read-first reopen/metrics/fetch has no migration SQL writes/new Class A. Charge common schema-query read rows.
2. Normal output PUT + three retirements: actual R2 attempts/bytes/callbacks unchanged; actual cursor written rows lose high-water updates; exact pre-delete temporary peaks match baseline. Repeated same-key PUT, no-peak PUT, empty repository and standalone owner delete must preserve exact counters and peak.
3. Owner invalidation/fallback: remove last witness, remove owner while another equal witness exists, failed fallback SQL, failed/ambiguous fallback sync, R2 delete failure, catalog-delete failure, partial retirement and retry. Assert no actual delete before necessary durable preservation and all debt outcomes remain truthful.
4. Extend existing Rust upload fault matrix (real PUT failure, catalog interruption, upload sync, CAS sync, lost/corrupt reply and partial deletion) with peak assertions and no fallback double-upload/CAS. Cover stale CAS orphan and shared-live-key preservation. Compare actual SQL cursor/R2 receipts, not inferred fake counters.
5. Two-phase **real Wrangler process restart** tests with identical persist directory: peak-carrier surviving retirement; all objects removed but fallback survives; old Native reconnect to v2; registry loss/unregister; orphan PUT raises peak then GC deletes it and a second restart still reports that peak. Add interrupted PUT-before-catalog restart, GC pre-delete inventory persistence and fault between inventory checkpoint/reset. Mere Native/DO object recreation is insufficient evidence of durable SQLite survival.
6. Legacy migration: preseed real v1 catalog/high_water; read without ALTER; candidate mutation adds column without data rewrite, preserves old peak; reconnect old Native under new Worker safely mutates v2. Test migration interruption/re-entry and record actual schema/cursor/page costs. Document old-Worker rollback limitation explicitly.
7. Fresh unchanged four-profile screen from selected preceding anchor, using the one common Worker and distinct Native registration-body source. Verify actual fixed and complete physical scopes separately; charge migration in whichever phase actually executes and all constructor/eviction scans. Actual fixed known cost must improve >=1%; all unchanged R2 bytes/live/peak, SQLite <=1%, 128 MiB snapshot, 16 MiB chunks/bulk, request/ref/cache/compaction, strict content, transfer and paired Git-normalized median/p95 guards must pass. No tests or projected row counts substitute for these receipts.

Adding one integer to roughly 11-13 retained catalog rows plausibly fits existing pages, but the current measured final database is 24,576 bytes: one extra 4,096-byte page would regress 16.7%. No SQLite same-page guarantee exists in this report. If real schema/page growth fails the guard, reject or redesign this candidate rather than waive the guard. Extra schema discovery and migration effects may also change latency/eviction; they require measurement.

Proceed only after reviewing this report and the current all-four-profile screen, choosing a clean anchor, and retaining the capability boundary. No architecture rewrite is required. The normal-path saving has a source proof; failure recovery, real migration/page receipts and a matched fresh screen are the concrete remaining work.
