# Focused independent R13 correction rereview

**Assessment: the original >256-key scalar-fallback blocker is resolved. No remaining material blocker was identified in this focused rereview.** This conclusion covers the narrow correction and its retained actual evidence. It does not establish performance acceptance, authorize integration, or supply a plateau/stopping conclusion.

Reviewed clean implementation `d84c2f6f85249ff7c6cb457f3e3f079b176e8b78` and its complete three-file delta from rejected freeze `6c1d1648f0d3db96e865bdc5ef5dca8545a1754e`. The original report is `/tmp/git-zero-deferred-catalog-source-review.md`; its P1 is superseded by the resolution below. The initial report's other source analysis and explicit retained uncertainty limits continue to apply to unchanged production code.

No source file was edited and no build, test, probe, service, network request, push, CI action or benchmark was run during this rereview. Only this report was written; JSON evidence was read offline with `jq`.

## Exact source continuity

HEAD was independently read as `d84c2f6f85249ff7c6cb457f3e3f079b176e8b78`, with an empty `git status --short`. Worker source tree is `22902cf4688aa7ee5da3705adceaaee0a2f08caa`; Native source tree is unchanged at `70c661265d559d1fd08ebb4970b0e239af70594f`.

The entire production correction is ten changed lines in `examples/repository-host/cloudflare/src/index.js`: an early eager-context branch for scalar garbage lists above the fused cap, plus an explicit eager-context exclusion from deferral. The other changes are eight documentation lines and the actual Rust-driven regression contract. Native, codec and private proof code are byte-identical to the original freeze.

Independently read production SHA256s:

| File, relative to `examples/repository-host/` | SHA256 |
| --- | --- |
| `cloudflare/src/index.js` | `dfa5d305b6dc511cdb85eb8b7ee717d09f70e7a0435160f29cecf75126c865c1` |
| `cloudflare/src/catalog-pages.js` | `6b230178afc1d6d9f8362b973b7274ff30a963db137ec783816c44e0928a27cd` |
| `cloudflare/src/retirement-proof.js` | `3ce5c71f08af064d65c712c38d64bf1c2ad75fbf861708d882f8bb14348a182c` |
| `src/cloudflare.rs` | `e9f15b71577a9073303d7f7647500d7ec89b0efbb8a07c072eedd768569d5e05` |
| `src/native.rs` | `b5019d36a372839ffe361d7657de0814557ce3185959648e0026d8c4da7c9a2e` |

The regression test hash is `6cc42612df5f489baef3ab2c6838f0f20b8196162973c9e7386c52a14c8d31a9`; the actual Worker fault wrapper and TOML hashes remain `9c8638708245f8c4c845d523144c24014c924f612e7e035e1c1cb275a32113ae` and `42dbdc46c62d9f4791f4dc2f1835ac9f4776558340e8c7fe478f9d4c72e32954`. Each is recorded identically in the new evidence `source.json`.

## Blocker resolution

`setDebtContext` now recognizes `Array.isArray(manifest.garbage) && manifest.garbage.length > MAX_RETIREMENT_KEYS` before calling the bounded fused validator. It retains `{generation, keys: [], eager: true}` and marks the context loaded. It does not build a larger debt/proof cache, raise a protocol cap, infer any physical absence, or reject the supported scalar manifest after publication. `retirePages` expressly excludes eager contexts from deferred survivor writes. Individual scalar DELETEs retain their immediate R12 persistence and synchronization path.

Existing publication order still resolves and syncs a prior bounded pending page before the scalar manifest can replace its marker. Reopen and generation-checked GC can load an oversized eager manifest, then complete authoritative recovery. A valid absence proof for such an oversized generation cannot be minted through the bounded fused routes; an earlier-generation proof fails the existing generation binding. Existing same-key and arbitrary-uncertainty limits are unchanged. This correction preserves the documented earlier R12 ambiguity guarantees for eager scalar operations rather than claiming garbage proves deletion or that arbitrary unrecorded PUTs are repaired.

## New actual evidence

Read `/tmp/git-zero-deferred-eager-257-4113605-1792297888254881372/eager-257.json`, its exact-source manifest, and the full new Rust contract. The report contains eight completed cases:

1. A 261-physical-object fixture first produces bounded pending retirement. Replacing its durable marker with a 257-key scalar generation performs no physical operation and charges one Worker/DO callback, five SQL read rows and two written rows: prior page acknowledgment and scalar CAS. The retained trace places the acknowledgment and its durable sync before repository-state publication; the catalog has 258 members afterward and remains certain.
2. Three immediate scalar DELETE cases each charge one real DELETE and one written catalog row, remove the exact member, return HTTP 204, and retain certainty. No HEAD, PUT or LIST is hidden in those calls.
3. A complete process restart during the oversized eager cleanup reopens at generation two with 255 physical objects and historical peak 14,248. A normal PUT succeeds after registry loss with one real PUT and one catalog write. Stale GC charges no R2 operations and zero SQL writes. Correct-generation GC charges one actual LIST, 254 actual DELETEs, 22 SQL read rows and 20 written rows, leaving two live objects, the historical peak and a certain catalog.
4. A further process restart followed by an ordinary PUT/CAS reaches generation three with three live objects. It charges two callbacks, one actual PUT and two SQL writes, with no HEAD/DELETE recovery or hidden pending debt.
5. The actual SDK facet constructs `HttpServices`, `DurableStateStore`, `R2BlobStore` and `Repository`, rather than reproducing their fallback logic manually. Its direct 257-key `try_compare_and_swap_retiring` returns `Unsupported` with zero Worker/DO/R2 calls and zero SQL reads/writes. `Repository::publish_retiring` then succeeds through the existing scalar fallback: 258 Worker/DO calls, exactly one successful CAS, 257 real successful DELETEs, 535 SQL read rows and 259 written rows. Its receipt contains the exact 257 distinct expected keys in order, all true. The empty physical/catalog result remains certain and preserves peak 14,062.
6. Another full process restart reopens that SDK namespace empty with peak 14,062. The same SDK bindings successfully perform a new real PUT and scalar CAS to generation two, charging two callbacks, one PUT and two SQL writes with no HEAD/DELETE. Empty garbage serialization is accepted in its canonical omitted or explicit-empty form.

These eight cases comprise the publication case, three DELETE cases, interrupted-restart/GC case, subsequent-mutation case, SDK fallback case and SDK reopen/mutation case. They directly cover the original failure mechanism and productive recovery. The 257-key stress fixture is deliberately outside the normal thirteen-key SQLite shape guard; it supplies no relaxed storage bound or score claim.

The parent reports that the initial fixture-only empty-garbage omission mismatch remains retained. This review independently checked the corrected assertion accepts canonical serde omission and does not change production behavior. The broader six R13 Worker controls and four Native faults are being repeated by their existing owners under the final production hash; this focused review did not rerun them or substitute earlier evidence for those final repetitions.

## Scope at completion

This rereview resolves the original source blocker at the exact clean implementation SHA above. A final evidence-ledger-only commit had not yet been supplied when this report was written, so no ledger commit is claimed reviewed here. Its source continuity may be checked separately without reopening unchanged code.

All four fresh profile screens and original-R1/final five-pair validation remain separate requirements. Worker CPU, DO active duration, retention, and the complete Native/cloud bill remain unknown. Review complete at the exact implementation SHA above; stop.

## Final ledger continuity check

Subsequently reviewed clean ledger-only HEAD `79011d7bc798c62cfd5f547aece65ca3f061f317`. Its complete delta from reviewed `d84c2f6f85249ff7c6cb457f3e3f079b176e8b78` adds only `docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r13-deferred-catalog-candidate/implementation.json`; no production or test source changed. Independently checked Worker tree `22902cf4688aa7ee5da3705adceaaee0a2f08caa`, Native tree `70c661265d559d1fd08ebb4970b0e239af70594f`, and implementation tree `83db94f211da13a9920fca025860eb2577fb16ef` match the recorded identities. Ledger SHA256 is `49a9e9c0ee9d1b906ef9bc9cff5a1f23e67adb1937af89c99a72352a7b79663a`.

All **54** recorded artifact path/size/SHA256 entries independently match retained files. All seven final Worker contract `source.json` files are byte-identical with SHA256 `b133228464aa451219ba690430a0f5b9a20c9b3421e2c23c9c5cefb440e56c6e`; every recorded production/test/wrapper/config module hash matches reviewed source. Their actual JSON reports contain 2 normal, 8 resurrection, 11 proof/compatibility, 5 flush/generation, 9 failure/uncertainty, 19 codec/storage and 8 oversized-scalar cases. The final Native source manifest also matches its recorded hash and unchanged source, and its report retains all four `delete-second`, `retire-catalog`, `retire-sync` and `cursor-unknown` cases. As before, Native's manifest omits the private proof module; the seven final Worker manifests bind that unchanged module. All four retained earlier fixture/manual-run roots named in the ledger exist with their artifacts. Earlier R12 evidence is correctly labeled supplementary under its earlier Worker hash; it is not relabeled final corrected evidence.

The ledger's copied normal cost, oversized fallback and realistic storage fields exactly match their final raw reports. The bounded three-call normal fixture changes actual SQL reads/writes from **9/4 to 8/3**, with the same three Worker/DO calls, one physical PUT, three physical DELETEs and 21 written R2 bytes. This supports one read row and one written row saved in that fixture. It supplies no measured four-profile saving, latency acceptance or full-bill claim. The final realistic thirteen-key shape remains 24,576 SQLite bytes initially/finally/reopened. The 257-key stress fixture is explicitly separate at 36,864 bytes before restart and 20,480 afterward, with no cap waiver. Unknown SQL remains NULL, receiptless intentional aborts retain incomplete accounting, and CPU/duration/retention/Native-compute dimensions remain unknown.

No new source audit, test, probe or service action was performed for this ledger check. The focused disposition remains unchanged: original P1 resolved, no remaining focused material blocker identified. This report reviews the isolated implementation and its ledger only; the parent's separately combined integration is outside its source identity. Ledger check complete; stop.
