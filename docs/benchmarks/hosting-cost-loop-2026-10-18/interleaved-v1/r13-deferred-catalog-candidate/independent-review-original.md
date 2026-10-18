# Independent R13 source review

**Disposition: do not approve frozen implementation `6c1d1648f0d3db96e865bdc5ef5dca8545a1754e`. One material compatibility blocker remains.** A correction and its focused actual evidence must be reviewed at a new exact clean freeze. No performance acceptance or stopping conclusion follows from this review.

This was an independent read-only review. No source edits, build, test, probe, service call, network request, push, CI action or benchmark was performed. Only this audit artifact was written. Existing JSON artifacts were inspected offline with `jq`. The complete R13 production delta from reviewed R12 `f90f54617bc4a4a1ff75ddab1870615163119fb4`, design requirements in `/tmp/git-zero-after-pages-cost-audit.md` and `/tmp/git-zero-deferred-catalog-design-review.md`, and the retained R12 correction review were read.

## Exact source identity

The worktree was clean when the exact implementation freeze was independently checked. Its Worker source tree is `f8affc2c980daf88347edf6cbfd5eda4a0cf77d4`, and Native source tree is `70c661265d559d1fd08ebb4970b0e239af70594f`, matching the supplied freeze.

Independently read production SHA256s:

| File | SHA256 |
| --- | --- |
| `cloudflare/src/index.js` | `0cebc70617bbf11107391186fa08bc706f809e743839bbeef98cbca104c5cb0c` |
| `cloudflare/src/catalog-pages.js` | `6b230178afc1d6d9f8362b973b7274ff30a963db137ec783816c44e0928a27cd` |
| `cloudflare/src/retirement-proof.js` | `3ce5c71f08af064d65c712c38d64bf1c2ad75fbf861708d882f8bb14348a182c` |
| `src/cloudflare.rs` | `e9f15b71577a9073303d7f7647500d7ec89b0efbb8a07c072eedd768569d5e05` |
| `src/native.rs` | `b5019d36a372839ffe361d7657de0814557ce3185959648e0026d8c4da7c9a2e` |

Paths in this report are relative to `examples/repository-host/` unless specified otherwise. Source line references apply to the frozen commit above.

## P1: supported oversized scalar retirement fallback commits and then wedges

`cloudflare/src/index.js:673–677` calls bounded `retirementKeys` for every current manifest with generation greater than zero. That helper rejects `garbage.length > 256` (`:121–125`). This incorrectly applies the optional fused-operation bound to the retained scalar manifest protocol.

The Native bridge explicitly returns side-effect-free `Unsupported` for a retirement set above 256 (`src/cloudflare.rs:875–877`; staged uploads also fall back before I/O in `upload_frame`). `src/lib.rs:330–345` then performs the supported original scalar CAS followed by individual scalar DELETEs. An existing repository can legitimately have more than 256 replaced chunks; this fallback is intentional, not malformed input or an attack.

With a loaded v2 catalog/debt context, the scalar CAS first transactionally publishes and syncs the next manifest. `publishManifest` then calls `setDebtContext(next)` at `cloudflare/src/index.js:1120`, after the durable publication. A 257-key manifest throws there, returning an error after CAS committed and marking the catalog uncertain. Scalar retirement never starts. Reconstruction repeats the same rejection while loading current debt (`:657–658,680–692`), leaving ordinary mutations blocked. Generation-checked GC can perform inventory/rebuild, but calls the same rejecting `setDebtContext` at `:1307`; it fails again, so normal Native recovery does not escape the state. This is a newly introduced compatibility regression beyond the retained R12 ambiguity limits.

Required correction: preserve oversized scalar retirement as an explicitly eager context with no cached unbounded debt list and no proof-based absence/deferral. Flush prior bounded pending retirement before its marker is replaced, preserve immediate ordered scalar deletion/persistence, and let constructor/GC accept the supported durable manifest. Do not raise the bounded fused/proof cap or remove Native's fallback.

Required focused actual Rust-driven evidence: publish 257 eligible keys through the scalar fallback with loaded durable v2 state; include replacement of existing bounded pending debt, complete ordered scalar deletions, eviction/reopen during and after that sequence, authoritative GC and successful subsequent ordinary mutation. Retain actual receipt/SQL/R2 counts and initial failing fixtures. No existing retained R13 contract below covers this above-cap transition.

The implementation owner independently confirmed the finding and proposed the bounded eager-context correction. That proposal is consistent with the protocol: a proof for the oversized generation cannot be minted by bounded fused routes, and earlier proofs fail the generation comparison. This is design feedback only; the correction is not reviewed or approved here.

## Other reviewed invariants

No further material blocker was identified in the stable source paths reviewed:

- The exact deferred capability and proof storage are distinct from existing generation-witness negotiation. Native keeps a fixed 178-byte lowercase-hex token in a mutex state whose entire allocation is charged inside the existing joint 32 MiB retained budget. Registration/unregistration/GC reset it; an epoch prevents a reply from restoring a token across those resets. Duplicate or malformed proof headers are discarded, and the token is cached only after strict typed retirement/upload response decoding succeeds with all expected outcomes true.
- The Worker proof uses a private registration HMAC key distinct from the Native-known generation-witness key, a private session, repository/domain/version, safe generation and SHA256 of the complete ordered eligible debt. Issuance checks the trusted matching DO reply, exact result shape/order, all true outcomes, one successful CAS, actual successful DELETE count, correct fused upload member/bytes and fully known SQL receipt. The actual validated receipt is charged once before optional body/crypto processing; optional failures suppress the proof without another dispatch. Registration object identity is checked across asynchronous work.
- Public proof headers and every internal header are stripped before service/gateway forwarding; trusted confirmation is injected only after verification. Oversized proof headers fail before DO dispatch. Invalid/missing/lost-session proof permits bounded actual HEAD recovery. Public generation witnessing still performs its authoritative read.
- Durable v1 and v2 codecs decode independently of registration state. V2 is 14 + 32×36 = 1,166 bytes, retaining the 1,286-byte cap. Full decoding validates framing, count/order/uniqueness/prefix, safe generation, zero-inclusive size through 16 MiB and row aggregate. Constructor SQL distinguishes both codec layouts for member counts.
- Reopen debt uncertainty is separate from sticky arbitrary catalog uncertainty. Debt HEAD recovery accumulates outcomes before changing live totals; proof/HEAD paths cannot clear arbitrary uncertainty. Reload discards volatile pending bookkeeping together with reloaded totals, preventing double subtraction. GC remains generation-checked and uses complete physical inventory with the pre-delete peak checkpoint.
- Same-key debt PUTs acknowledge and sync current debt before physical R2 PUT. Normal non-debt same-page PUTs can fold confirmed removals into their existing post-PUT catalog write/sync. Splits carry acknowledgment; GC rebuild acknowledges current generation. Multi-page/mixed/empty/overflow/unrelated-page/no-PUT publication paths flush eagerly, preserving the union-of-doomed peak rule.
- The final freeze includes the stale CAS-only precheck at `cloudflare/src/index.js:1095–1105`. A stale CAS with pending/unloaded debt returns before debt resolution or persistence. It charges the authoritative generation read and CAS receipt without HEAD/DELETE/PUT/catalog writes. Winning CAS still syncs before ordered physical retirement; upload catalog/sync still precedes CAS.

The metrics mismatch suppression whenever pending debt exists was examined. No independent new counterexample was substantiated within handled mutation paths: those paths resolve/acknowledge debt before reintroducing its keys. A counterexample presently requires the retained arbitrary uncataloged/external PUT gap, which this protocol does not claim to repair. Observer LIST does not resolve, flush, apply confirmation or mint proof. This review does not interpret a physical observation as new production certainty.

## Existing actual evidence inspected

The production Worker/bridge hashes in each of the six retained `source.json` files below match the independently read frozen hashes. The Native artifact matches the frozen index/pages/bridge/Native hashes; its source manifest does not record `retirement-proof.js`, so binding that module relies on the six Worker contract artifacts and frozen source identity.

| Artifact directory | Report inspected |
| --- | --- |
| `/tmp/git-zero-deferred-normal-4053429-1792297343665414743` | `normal.json`: 2 normal/control cases |
| `/tmp/git-zero-deferred-resurrection-4053942-1792297356241731965` | `resurrection.json`: 8 replay/barrier/catalog/sync/reopen cases |
| `/tmp/git-zero-deferred-proofs-4054266-1792297364242779050` | `proofs.json`: 11 proof/spoof/compatibility/session-loss cases |
| `/tmp/git-zero-deferred-flush-4054900-1792297380301018476` | `flush.json`: 5 no-PUT/stale-CAS/unrelated-page/split-GC/generation cases |
| `/tmp/git-zero-deferred-failures-4055306-1792297395698164119` | `failures.json`: 9 reply/member-validation/reload/partial/unknown-SQL/uncertainty cases |
| `/tmp/git-zero-deferred-storage-4055612-1792297405469118941` | `storage.json`: 19 codec/zero-size/shape/reopen cases |
| `/tmp/git-zero-deferred-native-4051927-1792297333458056402` | `report.json`: 4 Native fault/recovery cases |

The real thirteen-key, approximately 65 MiB shape reports 24,576 SQLite bytes initially, after retirement/fold/GC, and after full process reopen; the smaller churn store reopens at 20,480 bytes. These fit the retained 24,821.76-byte guard. This is correctness/storage evidence, not a performance score. Worker CPU, DO active duration, retention and the complete Native/cloud bill remain unknown.

Review stops at the exact frozen implementation above with the P1 finding unresolved. Final four-profile fresh screens and original-R1/final five-pair validation remain separate requirements.
