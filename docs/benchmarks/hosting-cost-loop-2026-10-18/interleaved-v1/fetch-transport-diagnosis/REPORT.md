# Bounded response-path transport diagnosis

All 28 original-Worker NoExpect response tests completed with HTTP 200 and exact upload/download lengths. The retained 138.538-second historical-fetch tail and the independent verifier's earlier partial 9,440,303-byte fetch timeout did not reproduce. This absence does not establish a fix or explain either retained failure. No production source, buffer policy, concurrency setting, benchmark result, deployment, or remote run was changed.

The separate explicit-Expect control reproduced the existing gateway HTTP 500. A temporary Worker copy that removed the forwarded Expect header let curl complete, but did not supply an interim 100 response to a strict client. That overlay is incomplete Expect handling and is not an implementation recommendation.

## Isolation and provenance

The frozen source was `/tmp/git-zero-hosting-cost-loop`, HEAD `457e2fa97b10cf57e47eea7e1c0fcb849d90780b`, before and after. Git status was empty before and after. The copied HTTP source is exact; instrumentation is confined to this temporary directory.

| Source | SHA-256 before and after |
| --- | --- |
| `examples/repository-host/src/http.rs` | `d41038c465141ddfc8cb7c839798798531225ffea248283ba743ee5605694fae` |
| `examples/repository-host/cloudflare/src/index.js` | `83b52ba0eabd9f752aa1c243ce285f20ab78caac9244329d00d0db788306f9bc` |
| `examples/repository-host/cloudflare/wrangler.toml` | `4d35c7af5e0725605b99801be715e53405689036a53c9df2682eba411cea353d` |

`SHA256SUMS.pre-run` retains source and executable hashes taken immediately before exercise. `SHA256SUMS` covers the completed evidence bundle, excluding transient state/configuration directories. Tool versions are retained separately: Rust 1.97.1, curl 8.15.0, Wrangler 4.149.0, and Node as recorded in `node-version.txt`.

The root verifier reported port 8828 stopped before exercise began. The Rust supervisor owned only the echo server process group on port 8830 and Wrangler groups on port 8829. It used `/tmp/git-zero-fetch-transport-diagnosis/state`, a fresh namespace `diag_fetch_tail_20261018_8829`, the installed Wrangler CLI under `/tmp/git-zero-hosting-fused-retirement`, and the frozen original Wrangler configuration. Registration omitted generation witnesses; no Git, R2, Durable Object, repository, or native codec operation ran. Registration was removed and owned groups were stopped at completion. The root independently confirmed their cleanup. No processes on 8828, 8798, or 8910 were stopped by this diagnostic.

The initial sandbox-only bind attempt failed with `PermissionDenied`, OS error 1, `Operation not permitted`; the exact error is in `raw/sandbox-bind.stderr`. The actual loopback exercise used the approved escalation. Runtime was 11,531.214 ms from registration through controls/cleanup registration, 19,376.731 ms including initial startup, below the 600-second exercise limit. Lifecycle records include process group IDs and completion; no more network probes were run afterward.

## Method

`driver.rs` is the sole request driver and runtime supervisor. It invokes curl as a subprocess; there are no JavaScript or Python benchmark runners and no added network dependencies. Python utilities only copied/instrumented source or analyzed already-written files offline. Wrangler's original JavaScript is runtime glue.

The echo server uses the copied integration HTTP runner with aggregate tracing and a counted writer. It retains serial accept/handle/write behavior, original 30-second socket timeouts, TCP_NODELAY, no-store responses, content-length framing, and connection close. Immutable response buffers are allocated before accept. Body bytes are synthetic; this exercise tests transport, not Git protocol or codec work. Instrumentation and immutable buffers can affect pacing.

Every response uses `application/x-git-upload-pack-result`. Normal inputs are 183-byte POSTs with Git request content type. Fourteen alternating direct/proxy pairs cover eight 34,203,505-byte historical responses, two 9,440,303-byte responses, two 47,000,000-byte controls and two 4,096-byte controls. Each curl process creates one fresh HTTP/1.1 connection, explicitly removes Expect, disables proxy use, writes output to `/dev/null`, and records connect, first-byte, request-upload completion, total time, status and exact byte counts. Correlation IDs appear only in safe URL query parameters.

The HTTP copy emits one aggregate record after each request, never headers, credentials or body data. Linux CLOCK_MONOTONIC timestamps share an epoch with driver start/completion timestamps: accepted, read start, headers done, request body done, handler done, response headers sent, and body writes done. Body write completion means the native socket accepted all response bytes; it does not prove downstream delivery. Client completion is separately recorded. The counted writer records successful wire bytes even on a failed write; errors and missing completion timestamps are retained. No per-read logging or TCP sampling was introduced.

## Original-Worker NoExpect results

These are diagnostic elapsed times, not benchmark acceptance results.

| Response bytes | Arm | Requests | Curl total range, ms | Maximum native body-write time, ms | Maximum driver completion after native write, ms |
| ---: | --- | ---: | ---: | ---: | ---: |
| 4,096 | direct | 2 | 0.199–0.568 | 0.015 | 0.955 |
| 4,096 | proxy | 2 | 4.773–7.524 | 0.021 | 4.255 |
| 9,440,303 | direct | 2 | 5.614–5.812 | 6.441 | 2.172 |
| 9,440,303 | proxy | 2 | 25.089–34.316 | 11.718 | 12.030 |
| 34,203,505 | direct | 8 | 7.918–19.741 | 18.354 | 2.314 |
| 34,203,505 | proxy | 8 | 88.150–404.549 | 110.997 | 24.372 |
| 47,000,000 | direct | 2 | 14.648–21.046 | 20.763 | 1.431 |
| 47,000,000 | proxy | 2 | 124.952–153.713 | 136.302 | 16.165 |

All 28 rows have curl HTTP 200, upload size 183, download size equal to the configured response, matching native body counts and no native write error. The largest original-Worker elapsed case, `p01_proxy_34203505`, took 404.549 ms in curl: 318.079 ms to first byte; 318.338 ms from driver spawn until native accept; 0.237 ms native intake; 77.662 ms native body write; and 13.456 ms from native write completion until the driver observed client completion. This does not reproduce the retained multi-second/minute tail or identify the cause of the pre-accept interval.

The first proxy response provides a concrete boundary example: Wrangler logged its POST as 200 in 12 ms, while native body writing took 91.098 ms and curl total time was 108.885 ms. The 34,203,505 response body produced 34,203,646 native HTTP wire bytes. A short Worker request log therefore cannot be read as response delivery completion. Raw logs preserve order, and `results.json` joins every ID to its exact stages and curl metrics.

## Expect controls and retained negative results

After the response tests, four original-Worker/direct controls used the same 8,543,657-byte synthetic input and a 4,096-byte response. Direct NoExpect, proxy NoExpect and direct explicit `100-continue` all returned 200 with exact sizes. Original proxy explicit Expect returned 500 after 9.639 ms, uploaded zero bytes and received a 278-byte error response. The runtime log identifies `Responses may only be constructed with status codes in the range 200 to 599, inclusive.` A separate strict Rust status-only probe observed `[100, 200]` directly and `[500]` through the original Worker. It records only status codes and counts, discarding headers/payload.

Only after those controls established the original failure did the supervisor restart its own port 8829 with `worker.strip-expect.js`. The entire Worker difference is one `headers.delete("expect")` beside the existing host deletion; a copied temporary configuration points to that file. The original Worker remained unchanged. Overlay NoExpect passed exact sizes in 73.724 ms. Overlay explicit Expect passed curl exact sizes in 1,025.051 ms, with request-upload completion at 1,015.745 ms and native intake lasting 1,020.103 ms.

That apparent curl success depends on curl eventually sending its body without receiving 100. The strict Rust overlay probe waited for an interim response and failed after its five-second read timeout with `Resource temporarily unavailable (os error 11)`; the gateway logged `Network connection lost` when the client closed. It never demonstrated an interim 100. This disproves complete handshake handling by the deletion overlay and corrects the assumption that the first hop already sends 100 automatically in this setup. Both the curl workaround and strict-client failure are retained, along with original 500 and native failed-request traces.

[RFC 9110 section 10.1.1](https://www.rfc-editor.org/rfc/rfc9110.html#section-10.1.1) permits a client to send its content without waiting indefinitely and describes separate server/proxy obligations for Expect. Curl's fallback success alone therefore does not verify the interim-response path. A complete gateway change would need an explicit, correct handshake across both hops and regression evidence; this diagnostic implements no such change.

## Limits and artifacts

The real historical fetch retained a 138,537.578 ms wall time versus 143.639 ms native upload codec time, 34,182,222 pack bytes and 34,203,505 HTTP body bytes, with no R2 reads or rehydration. This echo does not invalidate that result, the earlier upload-intake reproduction, or the verifier's partial-response timeout. It offers correlated traces for a bounded non-reproduction and a distinct Expect failure. No buffering, concurrency or production Rust change is supported by these results. There is no formal speedup, billed Worker CPU, Durable Object duration or isolate capacity conclusion.

`results.json` contains all 34 curl rows, per-stage joins, eight size/arm aggregates, strict-client probe outcomes and lifecycle. `raw/` retains 37 native request records, every curl JSON/stderr, original and overlay Wrangler logs, readiness failures, registration/unregistration responses, strict-probe results/errors, driver stdout/stderr and sandbox bind failure. Wrangler's default debug log displayed its configured local token; that known literal was redacted offline, with its original log hash and redaction count retained in `raw/wrangler-cli.redaction.json`. Request/response headers and payloads were never captured. `http.original.rs`, `http.rs`, `http.instrumentation.patch`, `server.rs`, `driver.rs`, both Worker sources/configurations, build versions, source status/hashes, and `COMMANDS.md` make the experiment reviewable. No error, timeout, control or prior report was discarded. `PUBLISH_FILES.txt` and `SHA256SUMS.publish` select a small reviewable source/log bundle, excluding binaries, large synthetic input files, runtime state/configuration directories and the verbose CLI debug log.
