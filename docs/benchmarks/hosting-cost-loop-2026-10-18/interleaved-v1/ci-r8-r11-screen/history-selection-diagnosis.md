# R10/R11 cold history selection diagnosis

Status: confirmed static classifier defect; no source changes, builds, tests, probes, services, benchmarks, pushes, or CI changes performed for this diagnosis.

## Finding

R10 rejects the normal Git 2.53.0 protocol-v0 fetch capability list. `Certificate::plan()` in `/tmp/git-zero-hosting-cost-loop/examples/repository-host/src/native/history.rs:225` accepts a short list of capabilities, but omits `deepen-since` and `deepen-not`. Encountering either returns `None` at line 242. `fetch_read_view()` in `src/native.rs:757` then takes the ordinary full-rehydrate fallback at line 769.

These tokens occur on ordinary, non-shallow real Git fetches:

1. The native advertisement policy explicitly advertises both in `src/native/advertisement.rs:16`. The real native advertisement must match this template for the observed R8 advertisement certificates/shortcuts to exist.
2. The downloaded provenance names Git 2.53.0, source commit `67ad42147a7acc2af6074753ebd03d904476118f`. The existing local checkout `/tmp/git-zero-upstream-source-2.53.0` is exactly this commit/tag, verified read-only with `git rev-parse HEAD` and `git describe`.
3. That Git source, `fetch-pack.c:1182`–1189, sets `deepen_since_ok` and `deepen_not_ok` whenever the remote advertises these capabilities. It does not require any shallow/deepen option.
4. `fetch-pack.c:393`–394 appends both capability tokens to the first `want` whenever those flags are set. The `want` is written at line 401.
5. Actual shallow requests are separate packet commands: `fetch-pack.c:414`–427. Merely echoing the two capability tokens does not request shallow history. On the server, `upload-pack.c:1120`–1127 handles the actual shallow/deepen commands separately; ordinary `want` capability processing at lines 1143–1172 does not activate shallow behavior for these two tokens, and line 1206 returns without shallow processing when no such commands were supplied.

This establishes a deterministic rejection in the production parser before pack selection. It does not require a runtime experiment to locate the defect.

## Frozen source and evidence

- Primary worktree stayed clean at `7e7856a3f6c279a8f97956a4475cb12f847b1f7e`.
- The R10 provider provenance is `e30e9c324fe12e9bbbd86959b1710cc9ea5b270e`.
- `git show e30e9c3:examples/repository-host/src/native/history.rs` and the frozen primary copy have identical SHA-256 `d37f9721f90eab0919c349bcd285dd9acfa42ba25834e3c105475d29c87d7e44`.
- Reports inspected: `/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r8-r11-screen/run-39092442019/profiles/16mib-cold/{r9,r10,r11}/16mib-cold.json`.
- Source/runtime provenance inspected in the sibling `provenance/` directory, including `r10-source.txt`, `git-source-commit.txt`, and `runtime.txt`.

Each measured sample (0 and 1) has the following final counters:

| Provider | Lineage bytes | Projections | Full restores | Cache misses | Cache hits | Rehydrates | Advertisement shortcuts | Actual R2 GETs | Actual R2 read bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| R9 | absent | absent | absent | 48 | 8 | 81 | 31 | 494 | 1,882,586,450 |
| R10 | 472 | 0 | 0 | 48 | 8 | 81 | 31 | 494 | 1,882,586,450 |
| R11 | 472 | 0 | 0 | 48 | 8 | 81 | 31 | 494 | 1,882,586,450 |

For each provider there are 32 measured historical rows across the two samples: 16 `history_no_haves` and 16 `history_with_haves`. All 32 miss the response cache and cause a rehydrate; R10/R11 perform zero projections across those rows. This rules out historical responses being served entirely from cache. R10 lineage-byte deltas across measured publications follow the expected bounded growth/compaction pattern and finish at 472 bytes, while all read deltas leave lineage size unchanged. The observed retained proof and 31 advertisement shortcuts rule out the earlier R8 CI environment/identity failure as this defect's explanation.

One concrete measured row, R10 sample 0 `history_no_haves_32`, wants the published `refs/heads/history-4` object `6d55590087ff6851e7e37884c864b854d1c43f51`; it is a cache miss, performs one full rehydrate, and reads all 11 packs / 39,265,016 bytes. Its request body has 183 bytes. The no-haves rows consistently have 183-byte bodies; seeded historical rows have 233-byte bodies (one extra 50-byte `have` packet).

Source-based reconstruction of the ordinary no-haves request is exactly 183 bytes:

```text
00aawant <40-byte OID> multi_ack_detailed no-done side-band-64k thin-pack no-progress ofs-delta deepen-since deepen-not agent=git/2.53.0-Linux\n
0000
0009done\n
```

Here `\n` denotes one LF and display line breaks between packets add no bytes. This is a reconstruction, not a captured packet trace. Git's `version.c:64`–78 adds the `-Linux` suffix when no explicit user-agent override is present. The exact 183/233-byte agreement is supporting evidence; the unconditional client emission and server allowlist mismatch establish the cause independently.

## Missing regression

The production integration test `/tmp/git-zero-hosting-cost-loop/examples/repository-host/tests/history_cloudflare.rs` sends history fetches using its hand-built `fetch()` helper at lines 31–38. The projection assertions at lines 285/294 pass only `ofs-delta`. Real Git pushes in that test exercise receive handling, not the real Git fetch capability negotiation. The native integration helper in `src/native/history_tests.rs:27` likewise uses only `ofs-delta`.

The supposedly realistic unit request in `src/native/history.rs:422` includes `agent=git/2.53.0` but omits both capabilities emitted by that exact client. Consequently it never exercises the offending branch. There is no existing real-Git historical fetch activation assertion covering this gap.

## Narrow correction and verification boundary

After explicit fix GO, the minimal candidate correction is to accept the exact `deepen-since` and `deepen-not` tokens as ordinary capability acknowledgments while retaining rejection of actual standalone `shallow`, `deepen`, `deepen-since <value>`, and `deepen-not <ref>` commands, unknown capabilities, protocol v2, compression/chunking, unknown OIDs, malformed framing, and the existing size/command bounds. `include-tag` remains separately unsupported; the frozen benchmark requests use `--no-tags` at `hosting-bench.rs:950`.

The missing regression should use a Rust correctness driver and a real Git 2.53.0 client through the registered production Native/Worker path. Prepare multiple measured-style publications and run `git -c protocol.version=0 fetch --quiet --no-tags --keep` from fresh and locally seeded clients with `fetch.unpackLimit=1`, then assert that historical reads actually increment `history_projections`, select fewer packs/bytes, and preserve correctness. Read `/metrics` for the immediate projection assertion; `/_object-stats` intentionally restores a full inventory and should be checked separately. Add a parser fixture with the complete real capability list and negative standalone shallow/deepen commands. No benchmark fixture, cost model, telemetry accounting, scoring, provider runtime, or accepted result should be changed to hide the failed optimization.

No corrected performance/cost claim is made. The downloaded failed R10/R11 evidence remains valid evidence that this implementation did not activate in these measured cold trials; it must be retained. Whole-screen acceptance remains pending all four profile/guard reviews.
