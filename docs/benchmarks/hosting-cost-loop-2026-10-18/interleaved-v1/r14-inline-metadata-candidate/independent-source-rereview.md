# R14 focused source rereview

Disposition: **source-only approval of `5759726b0a4545f6613553e1f2303f303eb65384`**. The previously identified P2 promotion issue is corrected and its focused actual-binding control passes. No additional source blocker remains from the bounded independent review. This permits isolated next-candidate preparation from this exact code SHA; primary integration, benchmark eligibility and final evidence-bundle continuity are separate pending work.

## Identity and correction

The worktree was clean at this commit. I independently verified Native source tree `bfc7e5dbf7e7f244572e5d0f40e79f3190ad1707` and Worker source tree `7df07b5796a98b066ee34f18bbd94f7aba66e1f1`. Final index SHA256 is `b07002d4bfa18b21fbb7b8c13e32f3919fffdaa5ad2e556ba388bc5d5142eb5e`; inline codec remains `b13edf1cc493e814b1b08b406c48b5544122bbaa6130e01d71d7e6555b2aec71`; SDK remains `fbed40632dcf4b653d4dd8f8c795723cf235263fd7250b8c89741b8dc8b54231`.

Removing exactly the new four-line acknowledgment guard from final index reconstructs SHA256 `eb7bfff3883a9ceabf2de70e90de58ab7dd87eca5b4f0c77e6d94fd4b804fa58`, the entire previously reviewed production index. Thus this production follow-up is precisely the reviewed promotion precondition, with no concealed additional source change.

`prepareInline` now compares the selected legacy-page acknowledgment against the already-authoritative state generation before encoding, ALTER or ownership-transfer writes. A future value marks `catalogCertain=false` and returns 409. Safe integer validation remains supplied by the page decoder/current debt context. The final bounded envelope, ownership, old API, debt barrier, coupling, failure recovery, proof and receipt assessments in the original review remain applicable with that correction.

## Actual focused control

Existing artifact `/tmp/git-zero-inline-future-ack-260924-1792300516587627166` records a sole normalized GZCPv2 root with a future acknowledgment and an empty-garbage generation-2 manifest. Its actual attempted upload returns 409 with the explicit acknowledgment error; measured SQL write delta is zero, physical PUT delta is zero, and Rust asserts zero total R2 class-A/class-B/DELETE work. The catalog retains its one normalized row, nullable inline root stays null, and uncertainty is set. The control uses actual paid demotion and SQL corruption in a test-only route, not production acceptance of caller-supplied root metadata.

Its current-generation GC then repairs the catalog; a normal subsequent upload succeeds, producing a certain three-object, 96-byte, generation-3 state with root H=96 and no pending debt. The control reopens the process and successfully reads generation 3. Final tiny SQLite is 20,480 bytes; this is a corruption/recovery fixture and does not replace the retained realistic 24,576-byte storage gate.

Every one of the 13 recorded source entries in its `source.json` exactly matches the clean candidate, including all four production Worker modules, SDK, Native/lib, changed wrapper and Rust test. The earlier retained pre-fix test fails with actual 500 rather than the required 409, corroborating that the new gate exercises the corrected branch rather than vacuously passing.

## Scope and pending evidence

The five earlier R14 Worker controls, six SAME Native fault controls and completed legacy controls executed the honestly recorded pre-guard production identity. Their successful results are not relabeled as tests of the final index. The focused guard gate above executes the corrected final source; source comparison establishes that the only intervening production change is the additional invalid-state precondition. Final actual production Git gateway-driver completion and the evidence ledger remain pending their own review. Library/Clippy/fmt claims are implementer evidence pending ledger continuity, not fresh commands run by this reviewer.

I performed read-only source/JSON/hash inspection, no build/runtime/probe/CI. There is no benchmark, net known-cost, faster-than-Git, full-bill or stopping-condition approval in this document. **STOP READY for a focused ledger-only continuity review once packaging is complete.**
