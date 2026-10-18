# Offline diagnosis of isolated R2-screen tails

Read-only analysis of published run **39081580940**, original scalar R1 versus R2 bridge batching. No benchmark, service probe, dependency install, test, production edit or commit was performed. Archives were extracted only into this analysis directory. The ongoing R3–R5 screen was untouched. `events.json` retains every measured event above 1.5 seconds, all seven failed guards with their worst three paired ratios, and per-group distributions; `source-locations.json` records exact measured-commit source locations. Raw array indexes below are zero-based; Wrangler log lines are one-based.

The strongest result is localization, not a proven runtime fix. Multi-second waits are mostly outside the measured native codec/storage phases. A distinct, source-proven duplicate SHA-256 pass is actionable for ordinary cold latency, but cannot explain the warm tails.

## Exact high-tail events

| Profile / provider | Raw index / sample / operation | Client ms | Native nonoverlapping stage sum ms | Gateway GET ms / POST ms / POST log line |
| --- | --- | ---: | ---: | --- |
| 16 MiB warm / R2 | 610 / 1 / history_no_haves_28 | 76091.385 | 81.996 | 3 / 86 / 2025 |
| 64 MiB warm / R2 | 230 / 0 / push_09 | 18976.049 | 165.108 | 3 / 18949 / 910 |
| 64 MiB warm / R2 | 252 / 0 / push_17 | 19167.107 | 189.821 | 3 / 19134 / 1086 |
| 16 MiB cold / R1 | 219 / 0 / push_05 | 22605.084 | 293.084 | 4 / 22568 / 1664 |
| 16 MiB cold / R2 | 219 / 0 / push_05 | 14260.690 | 269.810 | 6 / 14225 / 1065 |
| 16 MiB cold / R2 | 547 / 1 / push_05 | 15994.922 | 288.368 | 5 / 15960 / 2007 |
| 16 MiB cold / R2 | 250 / 0 / history_with_haves_16 | 60399.039 | 597.348 | 6 / 751 / 1378 |
| 16 MiB cold / R1 | 600 / 1 / history_with_haves_24 | 39858.562 | 425.254 | 4 / 625 / 4048 |

Native stage sums exclude inline maintenance as a separate addend because it is contained in storage. Exact component values are in `events.json`; rounded table sums may differ slightly.

Each repository namespace has exactly 88 raw operations and 176 logged gateway requests, in matching order: one discovery GET and one Git POST per operation. Namespace and order give an unambiguous sequence match, but the logs lack per-request correlation IDs and per-operation absolute timestamps. Service log durations are not disjoint additions to native phase timings.

The 76-second no-haves operation requested historical ref `history-15` at repository generation 28. Its paired Git operation is raw[518], 576.365 ms. The native handler generated 21,357,542 response-body bytes and the client installed 21,342,326 pack bytes; native CPU-side upload processing was 81.996 ms, client user+system CPU 440 ms, and actual R2 read bytes were zero. There was one response-cache miss and one discovery-cache hit, with exactly two native/gateway requests. Thus repeated successful application-level requests do not explain it. The missing approximately 76 seconds falls outside the observed codec/service/logged request durations. Response transfer/backpressure, connection waits, client pipe/index-pack I/O or scheduling remain possible; these logs alone cannot distinguish them or prove that all the delay followed the response headers.

The same R2 operation in sample 0 took 450.305 ms; R1 samples took 461.130 and 430.174 ms with the same installed pack size. The response exceeds the 16 MiB cache-entry bound, but that only skips insertion. All responses use the same fixed HTTP writer. R1 and R2 each had six 16 MiB-warm no-haves responses over the bound; only this one was slow. All 16 no-haves responses in each 64 MiB-warm arm exceed it and finish below one second. Therefore the cache threshold is not a demonstrated deterministic cause.

For the two 64 MiB-warm large-push tails, input bodies are 4,266,557 and 4,266,500 bytes. R2 read bytes are zero. PUTs at log lines 905 and 1081 each took 61 ms; nearby manifest and DELETE requests were small. Native stages total only 165/190 ms while the outer receive POST takes 18.9/19.1 seconds. This localizes the missing time within outer POST handling/transport and outside accounted Git/storage phases. It is compatible with the previously reproduced request-intake stall, not proof of the same mechanism. Earlier echo-only controls in the published `transport-diagnosis/REPORT.md` reproduced 6.65–6.69 seconds between headers complete and body complete with no Git, R2 or SQLite. No current probe was run.

## Framing, subprocesses and retry audit

The measured and current/scalar `src/http.rs` are byte-identical (SHA-256 `d41038c465141ddfc8cb7c839798798531225ffea248283ba743ee5605694fae`). Lines 46–109 parse an explicitly bounded content-length or chunked request; normal content-length bodies use `read_exact`, not EOF. Lines 133–160 accept serially, set TCP_NODELAY, impose 30-second socket read/write timeouts, emit exact `Content-Length` plus `Connection: close`, write the complete prebuilt response, then drop the socket. A timeout is per blocking socket operation rather than an absolute whole-command deadline; slow progressing transfers can therefore exceed 30 seconds. Send errors are ignored so disconnects do not kill the listener. No framing fault that produces a successful complete 76-second fetch was proven.

Native `command` writes stdin in a scoped thread while `wait_with_output` drains stdout/stderr; stdin ownership ends with that writer. `git_rpc` captures the complete Git response before constructing its Arc-backed body. The upload codec timer includes subprocess output collection, so the 82 ms observation rules out a 76-second server Git pipe deadlock. Cached and uncached bodies use the same response writer. The gateway forwards the incoming request body and returns the fetch response without consuming its entire body; it has no code that treats this historical ref differently.

The runner's `Command.output()` drains output and waits for the client; its timer covers the Git command and descendants, including transfer and local indexing. Native metrics, historical-client initialization/seeding, fsck and archive verification occur outside that timer. Top-level captured-output backpressure is not a demonstrated flaw.

A pristine local Git 2.53.0 checkout matches CI source commit `67ad42147a7acc2af6074753ebd03d904476118f`. Its `remote-curl.c` has a POST retry branch for reauthentication and forwards response data synchronously into fetch-pack; fetch-pack waits for index-pack. This admits client-side I/O backpressure. The recorded operation has only one GET and one POST, both HTTP 200; no application-level auth retry is visible. A pre-server libcurl/TCP retry cannot be excluded without curl tracing. The runner sets a 256 MiB post buffer, far above these request bodies; normal Git requests use content-length and suppress Expect. Git's low-speed controls default unset and are installed only when explicitly configured; there is no default 76-second low-speed retry in the inspected code. Inherited low-speed environment variables were not inventoried, so no claim is made about every environment value. [Git HTTP source](https://github.com/git/git/blob/67ad42147a7acc2af6074753ebd03d904476118f/http.c), [Git remote-curl source](https://github.com/git/git/blob/67ad42147a7acc2af6074753ebd03d904476118f/remote-curl.c).

## Failed latency guards

These retain the published raw medians/p95s and paired Git-normalized changes. History, large-push and recheck groups each have 16 measured operations; with the scorer's nearest-rank p95 the p95 is the maximum. Small pushes have 48 operations. A single tail can therefore dominate a screen p95; it must still remain a failed guard.

| Profile | Group | R1 host median / p95 ms | R2 host median / p95 ms | Paired ratio median / p95 change |
| --- | --- | ---: | ---: | ---: |
| 16mib-warm | history_no_haves | 201.98 / 461.13 | 209.48 / 76091.38 | -9.4% / +9631.9% |
| 16mib-warm | history_with_haves | 303.88 / 836.37 | 331.28 / 1012.39 | -0.1% / +40.4% |
| 16mib-warm | large_push | 215.22 / 640.15 | 230.88 / 1034.52 | +0.2% / +58.5% |
| 16mib-warm | latest_recheck | 24.39 / 28.11 | 24.81 / 45.44 | +0.8% / +60.5% |
| 64mib-cold | small_push | 246.95 / 311.54 | 275.88 / 343.05 | +13.6% / +9.9% |
| 64mib-warm | large_push | 226.42 / 1298.30 | 234.75 / 19167.11 | -6.7% / +2987.6% |
| 64mib-warm | latest_recheck | 25.15 / 34.12 | 25.12 / 75.64 | -1.9% / +132.5% |

The 64 MiB-cold small-push median is a separate consistent concern, not a huge isolated warm tail. Native receive preparation median rises **202.902 → 230.730 ms**, while codec/storage/publication medians remain about 15.9/5/2.4 ms. Both arms read exactly **2,382,410,468 R2 bytes** across these 48 pushes.

## Source-proven bounded follow-up: duplicate chunk hashing

Measured R2 `src/cloudflare.rs:72` `decode_bulk_response` verifies each chunk's content-addressed SHA-256 at line 103, then returns `Vec<Vec<u8>>`. `HttpServices::r2_get_many` returns those values through R2BlobStore/HostBlobs to `Repository::read_snapshot_files` (`src/lib.rs:122`), which calls `verify_chunk` again at line 161 before materialization. Oversized scalar fallback in the same bulk getter also verifies before the repository's second pass. Original scalar R1 reads and verifies each chunk once. Current integration retains the same duplication (`cloudflare.rs:119`, `lib.rs:161`).

This is proven redundant work, not proof that it accounts for all the observed 27.8 ms preparation increase. It is a reasonable bounded latency candidate: establish one explicit verified-result boundary with a private constructor that validates complete metadata, framing, expected lengths and hashes once; preserve direct bridge corruption rejection, ordered/repeated-key semantics, legacy scalar fallback, and mandatory repository verification for untrusted/default BlobStore and CloudflareBindings implementations. Obtain a clean independent design before implementation; do not simply delete the outer verifier or blindly remove bridge integrity checks. No code was changed here. Eliminating this native duplicate pass does not itself lower the currently scored known cloud dollars.

Crucially, warm historical fetches and the two warm large-push tails perform **zero bulk/R2 reads**, so this duplicate-hash finding cannot explain their seconds-long waits. No response-buffering, accept-concurrency, socket-buffer or timeout change is justified as a proven latency repair from these data.

## Random-history algorithm and read amplification remain relevant

R2 preserved actual R2 GET counts/read bytes while reducing bridge requests. Cold historical reads still rebuild the full published snapshot. Across the 16 measured historical operations per group, R2 reads **383,394,216 bytes** for 16 MiB no-haves and the same amount with haves, against generated responses of 205,070,378 and 85,426,372 bytes: **1.87× / 4.49×**. At 64 MiB those groups each read **795,959,888 bytes**, against generated responses of 615,583,841 and 85,426,552 bytes: **1.29× / 9.32×**. These are group-specific ratios; the scorer's full varied-fetch amplification includes latest reads and is a different aggregate.

`cloud_read_view` rebuilds every published pack for cold requests; discovery can rebuild too, while a response-cache hit avoids rebuilding. The workload intentionally uses cold misses, so shrinking actual required object/index reads is still a substantive future design target. A cache-policy change would alter the objective and is not an equivalent optimization.

Warm history-with-haves native codec medians remain approximately **188–189 ms**, versus no-haves **35 ms (16 MiB)** or **106 ms (64 MiB)**. Native Git uses `pack.window=1` for haves and `pack.window=0` otherwise. This ordinary negotiation/delta work and absence of equivalent native MIDX/bitmap maintenance remain relevant to steady performance; none explains the current missing 19–76 seconds. All conclusions concern the measured local hybrid topology with native Git outside workerd, not production Worker codec speed or billed CPU.
