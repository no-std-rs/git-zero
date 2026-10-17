# Bounded local transport diagnosis

No production source was edited. Tests used temporary Rust echo servers built from the exact integration `examples/repository-host/src/http.rs` with timestamp logging added only to copies in this directory. The Rust handler returned the request body length; it did not invoke Git, R2, SQLite or repository code. All temporary processes were stopped and the two diagnostic registrations removed before independent timing could resume. The existing port-8808 emulator was not restarted or stopped.

## Evidence

Original HTTP source SHA-256: `d41038c465141ddfc8cb7c839798798531225ffea248283ba743ee5605694fae`.
Instrumented original copy SHA-256: `db895302787d52ced27e3aef207250cb57c2edfd7b68520bc79496f23c0034fc`.
Compiled echo server SHA-256: `5138017016edecd12a216c85eb593db9069692072d81f0eec2e1f139a1f53535`.
Rust: `rustc 1.97.1 (8bab26f4f 2026-07-14)`; curl: `8.15.0`.

Bodies had the exact benchmark initial-request lengths, 2,136,262 and 8,543,657 bytes. They were deterministic repeated byte patterns. Curl explicitly removed `Expect`; content type matched receive-pack. The existing Worker gateway forwarded the body to the temporary Rust echo server registered in its own diagnostic namespace.

First six gateway uploads of 2,136,262 bytes took 7.781, 7.846, 6700.790, 6659.525, 212.976 and 8.179 ms. Three direct Rust uploads took 1.097, 1.010 and 0.892 ms. Six gateway uploads of 8,543,657 bytes took 16.751, 16.396, 19.806, 24.032, 22.308 and 19.646 ms; three direct uploads took 3.178, 3.072 and 3.313 ms. Curl summary values were returned in tool output; durable server timing evidence remains in `server.stderr`.

The two long requests show:

| Request | First line complete | Headers complete | Body complete |
| --- | ---: | ---: | ---: |
| Slow 1 | 0.237826 ms | 0.278232 ms | 6692.086144 ms |
| Slow 2 | 1.122964 ms | 1.155606 ms | 6654.082730 ms |

Times begin immediately before the first request-line read. These reproduce the measured benchmark's 6.7-second size-specific stall in request-body intake, with no codec or storage involved. They are not an idle-accept reproduction. They do not distinguish upstream intake from forwarding or downstream TCP behavior; no TCP queue/window sample was captured during either stall.

Subsequent original-server uploads did not reproduce the long tail. A second copy logging every individual body read also did not reproduce it; that instrumentation can change stream pacing and does not demonstrate a fix. Explicit `Expect: 100-continue` separately caused gateway HTTP 500, not the observed stall; the gateway forwarded the header and the Rust server emitted its interim response. This is outside the measured Expect-free reproduction.

## Fresh Worker control

A temporary minimal Worker on port 8818 compared direct streaming forwarding against bounded request-body buffering before forwarding. `worker.js`, `wrangler.toml`, `wrangler.log` and `curl-throwaway.stderr` retain its source and traces. Eight alternating pairs all returned HTTP 200. Streaming totals ranged 16.940–207.167 ms; buffering totals ranged 46.424–201.279 ms. Neither arm reproduced the 6.7-second tail. Buffering is therefore not an evidenced remedy.

## Recommendation

No production Rust latency fix is justified by current evidence. In particular, changing serial accept/handler dispatch does not target the body-intake stall that was reproduced. Preserve the original latency failures and the separately localized 7.7-second R2-bridge append stall.

The smallest useful Rust change is opt-in HTTP transport tracing: record monotonic accept/read-start/header-complete/body-complete/handler-complete/write-complete timestamps, body length, method and path, and the longest body-read wait; never log credentials or body bytes. Aggregate per-read metrics in memory and emit once per request to reduce pacing perturbation. Verify content-length and chunked bodies, premature EOF, Expect handling and fragmented writes in a deterministic loopback framing test. Add request correlation and capture curl upload-complete timestamps in an isolated reproduction before choosing a transport fix.

Formal acceptance still requires the fixed independent quiet-window reruns. If a transport correction is subsequently proven, freeze and commit/push equivalent baseline-plus-correction and candidate-plus-correction controls, retain every previous report, and repeat all four five-pair profiles without changing objective, limits, cache policy or guards. Local elapsed time never supplies billed Worker CPU or DO duration.
