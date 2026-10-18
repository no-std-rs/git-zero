# Independent R16 final ledger continuity review

**APPROVED for ledger continuity and the bounded actual correctness/topology evidence described here. No proven blocker found.** Exact clean ledger HEAD is `fc47c3c1bba1c80564c585141b7f70d5171f3a46` in `/tmp/git-zero-hosting-small-receive`. Production remains the previously source-approved `603b224eda3719c7525db00ac6a112a71368d7b3`; `a624a789b98570fabd324ab0a16b803daa4641be` changes only the Rust test driver; the final ledger commit adds only its six evidence files.

This is **not integration/promotion, net-cost, latency, benchmark-validity or stopping approval**. It does not rehabilitate the current invalid cloud comparison or transfer old measurements to R16. Fresh authorized screens/all-four-profile guards and the mandatory fresh original/final five pairs remain outstanding. Full bill stays NULL; Worker CPU, DO duration, monthly SQLite retention and Native compute remain unknown.

The review was read-only except for artifacts under `/tmp`: no builds, tests, services, probes, performance runs, reimplementation, source edits, integration, push, CI, deployment or delegation. Primary source/current benchmark artifacts were untouched. Exact source was not changed.

## Published identity and exhaustive byte checks

I independently re-read **all 4,781 listed local run files totaling 2,300,886,900 bytes** and compared every length/SHA256 to the archived global inventory. All pass. Per-run inventories have disjoint published/unpublished partitions whose union exactly equals each local inventory; the 15 inventories reproduce the global map with no missing or duplicate path.

I re-read all **2,412 capsule members**, checked unique safe regular-file names, and computed a per-member hash inventory. All 2,335 inventoried published run files match their exact archived lengths/hashes; 15 additional runner logs match their external original logs; 14 reference files and 31 historical snapshot files match their retained originals. The remaining 17 members are the 15 inventories, global inventory and explicitly reconstructed early restricted-library summary. No capsule member was left unread.

All six published files equal their bytes in clean `fc47c3c1bba1c80564c585141b7f70d5171f3a46`. All five payload hashes in `SHA256SUMS` pass; the checksum manifest itself is independently hashed and pinned by the commit.

| Published file | Bytes | SHA256 |
| --- | ---: | --- |
| `README.md` | 916 | `ebce943f99b644c1e9d68540b58598e0cb78d6cb574ef838b492d3339af84943` |
| `SHA256SUMS` | 421 | `7bd70dafc2cc080f663529e26c375facb54b17b0a6eb9e4be50c1577f3ad0383` |
| `cleanup.json` | 320 | `3854ff27254e7a36a79d60c2c5a17c912689c93698954b441ada7c7f02ebc17e` |
| `implementation.json` | 14461 | `d88ce9a7369734ce63de3cbbedd7b29cc315ac4c1ff5b2bfdac77f800cfe7633` |
| `publication-validation.json` | 389 | `d995046d221a9a7e39c2a1ec800175964c144f3d4bf4e92ecb987b2d7b2fb87a` |
| `raw-evidence.tar.gz` | 1522572 | `d3a4f1aeb5cfc9634e8ed9b6e44cd8c4b139db006340c8aa9502986a98d8a391` |

Total published size is 1,539,079 bytes. The implementation ledger digest is `d88ce9a7369734ce63de3cbbedd7b29cc315ac4c1ff5b2bfdac77f800cfe7633`; capsule digest is `d3a4f1aeb5cfc9634e8ed9b6e44cd8c4b139db006340c8aa9502986a98d8a391`.

## Exact source, binaries and raw pre/post continuity

The Native tree `a255cf03e52648248cd0e71051a3c2a9d8e0a827` and Worker tree `0f35e4e9ebfcfe944ee9e3ba235f6bd6a901630f` are identical at source603, test-only a624 and ledger fc47. I checked all 30 manifested source/config/test files in each of the **eight commit-labelled successful runs** against the exact declared Git commit. Each 32-entry source/binary map has byte-identical before/after files and the ledger's exact advertised manifest digest.

The actual current final Native bytes match SHA256 `987be253e92afe3de5124f6651d7809180411b402f674bd7470d0ffd4c4f8b97` (26,828,976 bytes); the final a624 Rust runner matches `29141a37e955abe68671631a014379459f58d5cf12450c45cd254fc062fab71b` (26,814,712 bytes). Exact603 gates consistently record runner `1de1cbb5f68c9fef451c7ef0acd08f164bb72f38ef606f03763d07faec6f2191`; that overwritten historical runner is not falsely claimed to be the current binary. Native bytes are unchanged across the test-only extension.

I reconciled **512 embedded metric snapshots** in contracts/receipts against retained raw HTTP body snapshots. Integers, strings, nulls, flags, identities and all receipt values match exactly. Repeated serialization of floating timing values yields 419 last-bit differences, maximum `7.105427357601002e-15`; these are explicitly recorded in the continuity audit, not silently counted as byte-identical metrics. Original raw bytes are separately hash-verified.

Seven earlier runs retain historical identities: the two failed activation attempts; successful pre-hardening candidate/control; and three failed receiving-wrapper/expectation attempts. They have their own source-before hashes, binary maps, logs and inventories; absent after-maps remain null. They are not relabelled as exact603 evidence. The separately recorded early restricted-library failure explicitly says its summary was reconstructed from the tool transcript and its original stdout was not retained; this review does not invent missing raw output or overwritten historical binary bytes. The earlier provisional review and its hash remain preserved in the reference/snapshot records.

## Actual topology and relevant failure evidence

I independently recomputed all **eight final exact603 on/off pairs** from raw before/after counters: seven real small Git pushes plus a projected-to-Full small receive. Each activated pair removes exactly one service Worker and one DO callback, with equal Full GET count/read bytes, SQL/R2 writes/deletes/bytes, logical input/output and one cold reconstruction; fetch cache misses stay zero. The projected receive starts projected and ends Full in both modes. Six retained response bodies are byte-identical, including local large pack responses of about 9–20 MB. Those large local bodies were actually read here; their publication limits remain explicit below. Empty-probe/default-large-push controls retain ordinary receive behavior and the single cold restore. Warm controls retain zero gateway prefetch/rehydration and five verified fetches without R2 reads/writes; the separate inherited fetch matrix retains 23 cases.

I checked all **15 exact603 private receiving fault/race rows**, their status headers and actual first-read trace counts. Plan/auth-before-dispatch and generation mismatch cases have zero first GETs where required; paid continuation/corrupt/body-failure cases retain positive attempts. Paid local-change discard is counted. Missing/body receipts preserve incomplete accounting; unknown SQL remains null.

I independently checked the nine publication/preparation rows in exact603's full gate and the nine focused a624 rows (**18 rows across two gates**), including actual PUT, coupled SQL/transaction rollback, durability-sync/lost replies, partial deletion, unknown SQL and suffix-read failure. Begun Native operations count one logical request and their actual Native output once: errors use 22-byte 503 bodies; successful Native replies lost later at the gateway retain the actual 41-byte Native output and external Worker500 distinction. Attempted CAS is zero or one as appropriate; no hidden automatic receive/CAS retry appears in the recorded traces.

Required dirty failure followups use the same driver's single Native lifetime, continuous Native/Worker counters and recorded physical `r2_list` recovery **before** the next PUT. A completed Native reply lost only at the gateway leaves Native knowing publication succeeded; its next ordinary push correctly does not require dirty recovery. Unknown-SQL success likewise is not falsely treated as a failed publication. Same-Native assertions are supported by the exact driver lifetime and continuous counters/traces, rather than an invented retained PID field.

The focused a624 controls additionally prove three explicit manual retries use byte-identical receive payloads and the **same immutable PUT key** after after-PUT/pre-SQL/transaction failures. Each has physical GC before that next PUT. These deliberate subsequent client requests are distinguished from forbidden automatic gateway retries.

Fresh isolated uncertainty controls avoid sticky combined-trace ambiguity:

- `unknown-focused-01`: SQL read/write `127/19 -> NULL/NULL`, accounting completeness stays true with no receipt error; the next operation retains NULL. No missing-row estimate is filled with zero.
- `suffix-focused-01`: completeness `true -> false`, remains false after recovery, and records the missing/invalid receipt error. The failed prepared operation counts one logical request, 503/22 bytes and paid first-read work; the same Native later performs physical recovery before PUT.

After an injected **after-actual-PUT** reply failure, the binding receipt truthfully records one attempted failed PUT and zero *acknowledged* written bytes. The wrapper plus later inventory/recovery establish a physical write occurred; that zero must not be interpreted as zero physical transfer. Likewise a lost partial-body receipt does not prove no hidden work. These invalid fault traces are not priced as complete successful benchmarks. Known-or-NULL SQL and incomplete receipts remain visible. Actual focused followups retain SQLite at or below 24,576 bytes and owning cache/metadata at or below 32 MiB; no cap was raised.

## Cleanup and publication limits

`cleanup.json` is hash-verified and records no owned live processes or listeners, with all owned watchers stopped at 2026-10-18T06:41:17.450147+00:00 for ports8837/8838. Rust-owned child/group cleanup and completed runner logs are retained. During this read-only review, `/proc/net/tcp` and `/proc/net/tcp6` show no listener on those ports. The sandbox denied `ss` netlink access, so I do not claim an independent fresh host-wide process enumeration from its isolated PID namespace. No process or service was changed by this reviewer.

The compact capsule intentionally **does not publish** all 2.3 GB of repeated fixture/Git/R2 pack payloads, large response bodies or Native/runner executable bytes. They are distinguished as retained locally, with complete per-run path/size/SHA inventories. I verified all listed local files now and separately checked current final binaries, but another reviewer without this workspace cannot validate omitted bytes from the 1.52 MB capsule alone. Deterministic Rust fixture generation and source/config/tool identities support reproduction; neither omitted historic source/binary bytes nor absent original stdout are represented as published evidence. This limitation is plainly stated in the ledger and is not a continuity blocker.

## Independent audit records and disposition

Machine-readable approval: `/tmp/git-zero-small-receive-final-ledger-review.json`, SHA256 `a4e7f41d6aaba10006ec31f9ce4cc745261421cf1abcf1870f0f5794c0c50ab3`. Supporting independent inventories/audits are:

- `/tmp/git-zero-small-receive-final-ledger-hash-audit.json`: `25120d606c0f444c2897079110da04fda8fba46f0db9970771e04b1ae38abdb4`.
- `/tmp/git-zero-small-receive-final-ledger-continuity-audit.json`: `2fca388c8f5abab403a958ebbca357a142c1e0d8f2c364823c823d0cf548085e`.
- `/tmp/git-zero-small-receive-final-ledger-receipt-audit.json`: `ff9871700125e8e148993abfa9a128c921d90e7d0e45b6d71a4f4aba9835d73b`.
- `/tmp/git-zero-small-receive-final-ledger-pair-audit.json`: `dc3f549ab642e2bf42fef7002d3c6f03dcfd43b3752e04f70269bb9bb78ec64a`.
- `/tmp/git-zero-small-receive-final-ledger-private-cleanup-audit.json`: `80f3986234f435bf4f3c9acc4917d7a24fbbee260b3020cc3318468050660dfc`.

**Ledger continuity APPROVED for exact `fc47c3c1bba1c80564c585141b7f70d5171f3a46` and implementation digest `d88ce9a7369734ce63de3cbbedd7b29cc315ac4c1ff5b2bfdac77f800cfe7633`.** No integration/push/performance action is authorized or performed by this review. No measured R16 net-cost, latency, plateau or stopping claim follows. Written 2026-10-18T07:12:27.582146+00:00. **STOP READY.**
