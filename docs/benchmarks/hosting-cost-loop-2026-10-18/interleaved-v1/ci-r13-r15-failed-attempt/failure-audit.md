# R14 warm upload failure: bounded read-only audit

READY. The retained 64 MiB warm artifact establishes a failed excluded R14 warmup, before any R14 measured pair or R15 trial on this profile. It does not establish an R14/R15 publication, durability or resource-accounting bug, or a specific transport cause. This run cannot supply a comparable all-four-profile cost/performance decision. No source, binary, service, CI or benchmark changes were made by this audit.

Actual compact timeline, receipt snapshots, source hashes and limitations: `/tmp/git-zero-cloud-upload-failure-audit.json` (SHA256 `0e108747b63cce640f9e449bb66affcc1d4f012b795e86919e27c44a7ffb2d1b`). Scope is only the first finished profile, not other still-running profile VMs. Independent measurer owns complete archive/scoring review.

## Evidence and identity

- Run `39104016332`, attempt 1; trigger and primary source frozen at `82b64319bf9d39958c861b6fb7a9a6bae25fda9b`.
- Original archive `downloads/hosting-screen-64mib-warm-1/hosting-64mib-warm.tar.gz`: 433,417 bytes, SHA256 `d69e7f3322fe41df87b083bcfeff874245af88cd566efb5c8360adc13ca33c8b`. Independent measurer reports all 37 internal hashes pass. This audit read the extracted original files.
- R14 Native pin `5759726b0a4545f6613553e1f2303f303eb65384`, Native source tree `bfc7e5dbf7e7f244572e5d0f40e79f3190ad1707`; actual Native binary SHA256 `f7a8a971e509422a71cadcc1ff87531200ca6987a19e59b2e32af4a5bffa01ef`.
- R13 pin `774adce8795577f4ed7f4a21c3239106abeb21e0`; R15 pin `5a7f74f58336b5474dc9920cd20857ab8e573910`. Common five-module Worker tree `2944d3ecf512c09452e9d4487bd8bfc0b3815f91`; all five recorded module hashes are preserved in the JSON.
- Actual npm provenance and frozen lock agree: Wrangler `4.149.0`, Miniflare `5.20261006.1-alpha`, workerd `1.20261006.1`, Node `22.23.3`, Git `2.53.0`. Lock integrity and URLs are retained in JSON. Exact Wrangler package source was not available in the inspected checkout; attempted read-only retrieval failed DNS/web accessibility. No older Wrangler implementation was substituted.

The first R13 full warm suite completed from 06:29:21Z to 06:40:36Z, exit 0, with all 624 raw rows retained for warmup and samples 0/1. R14 started at 06:40:42Z and exited 1 at 06:41:15Z. Its journal contains 33 successful operation rows, their 33 verification rows, and one failed operation: sample -1, `push_13`, atomically publishing `main` and `history-13`. No R14 measured pair or R15 trial ran on this profile.

Git recorded HTTP 503 with unexpected disconnect. Wrangler recorded one failed ProxyWorker POST to `/_services/manifest/upload-retire` with `Network connection lost`. The private Wrangler log records an initial reloadComplete at 06:40:37.707Z and matching isolate ID heartbeats through 06:41:07Z. It contains no correlated body/dispatch trace, failed-request timestamp, runtime restart cause, OOM, assertion, or crash evidence. Initial startup teardown/reload entries precede the benchmark; they are not failure-time reopen evidence.

## Actual failed-operation observations

| Dimension | Before | After |
| --- | ---: | ---: |
| Manifest generation | 12 | 12 |
| Current SQLite bytes | 20,480 | 20,480 |
| Actual binding PUT attempts/successes | 14/14 | 14/14 |
| Actual binding DELETE attempts/successes | 6/6 | 6/6 |
| Actual CAS attempts/successes | 12/12 | 12/12 |
| Push Worker gateway requests | 24 | 26 |
| Push Worker service requests | 14 | 14 |
| Push DO requests | 38 | 40 |
| Push SQL rows read/written | 73/19 | 75/19 |
| Native compactions | 2 | 3 |
| Native view | full generation 12 | invalid |

Worker epoch remains `1dc171eb-bd8a-4f1a-ac65-07064bc8f383`; observed DO epoch remains `436b2c04-5e4d-4509-9a98-7dcbfa882941`. Both snapshots report accounting_complete true and no accounting errors or binding failures. Complete raw snapshots in JSON also retain observer effects: one extra stats LIST and the two gateway generation reads explain aggregate advances. Observed unchanged publication/binding counters do not prove that every internal component stayed alive or that no work was in flight elsewhere.

Native compaction before upload and invalidation after a publication error are expected source behavior. `publish_staged_retiring` propagates ambiguous upload errors; only pre-I/O Unsupported permits scalar fallback. Native marks its view dirty before attempting publication and invalidates it on error. No retry or scalar republish is justified.

## Body, framing and accounting boundary

Native `cloudflare.rs::upload_frame` serializes metadata and the pending chunk into one GZUP frame. Native and Worker agree on a 16,779,276-byte wire cap, a 16 MiB chunk cap, 1,500,000-byte metadata cap and 256 retirement keys. Native decides bound/capability fallback before I/O. A near-maximum chunk with too much metadata uses the existing scalar path; the source does not send an over-bound fused frame. The actual failed frame length was not recorded. The 64 MiB fixture size is not a service-upload frame size.

Native `HttpServices::call` connects one TCP socket to the Wrangler listening authority, writes a complete HTTP/1.1 request with exact Content-Length and Connection: close, then reads one response. Socket read/write timeouts are 60 seconds. The native Git HTTP listener separately allows a 64 MiB Git request body and uses 30-second socket timeouts. No retained timing/error identifies either timeout as this failure's cause.

The topology includes Wrangler's ProxyWorker between port 8808 and the user Worker (initial private log user Worker URL 127.0.0.1:39169). Git enters the user Worker gateway, reaches Native, and Native callbacks return through port 8808/ProxyWorker to the same application service gateway, DO, R2 and SQLite.

Exact common Worker `index.js` order:

1. Parse URL/repository and classify service route.
2. `workerCount(... service ...)` at line 410, before token auth at 411.
3. Optional proof verification and full bounded request-body buffering at line 467.
4. Construct the buffered Request and call `dispatchService` at line 480.
5. DO `uploadFrame` buffers/validates frame, shape, lengths, generation and content hash before PUT/CAS.

Thus an ordinary app-entry body-read/auth failure should already increment the app's service counter. Body-read exceptions before DO dispatch do not mark accounting incomplete, but they precede DO I/O; service entry is still counted. A lost DO dispatch explicitly marks accounting incomplete. No specific omission of a billable Worker invocation during app body intake is proven here.

The observed zero service-count advance is consistent with a callback failing before counted application entry. It is not proof of zero outer Proxy invocations or zero billable work. One outer Proxy POST attempt is recorded, while unrecorded proxy/isolate work and billing remain unknown. The local app receipt scope does not turn that missing observation into a zero. This is an invalid trial with unknown failed-request dimensions, even though successful prior operations have complete receipts.

## Proven evidence-retention gap

The common Rust runner redirects each Native child's stderr into its fixture trial's `host.log` (`hosting-bench.rs:190`, `:821`). Native prints the concrete propagated error there (`native.rs`, `hosting request failed: {error}`), then returns a generic 503. Failed fixtures are retained at `/tmp/git-zero-host-bench-25260-1792305642731011659` on the VM. The CI archive step packages only REPORT_ROOT, explicitly excluding fixture/state/binaries. No `host.log` is present among the 37 archived files.

This is a proven diagnostic/evidence-retention limitation, not proof of a data-publication fault. The actual private Native error cannot be recovered from the current archive or the now-destroyed finished VM. Also, `set -e` after R14's nonzero status skips end-of-timing source/binary checks; initial frozen provenance remains, but this profile has no successful final verification record. No source drift is inferred.

## Immediate handling and bounded next diagnostic proposal

Preserve the original archive, incomplete JSON, full progress journal, both Wrangler logs, successful R13 suite, hashes and failure snapshots under attempt 1. Do not replace this attempt, drop the warmup failure/tails, retry the ambiguous write, increase body caps, promote a provider, or count this attempt as a failed viable cost design/stopping condition. Other profile VMs continue undisturbed. The current source/branch stays frozen. R16/R17 isolated preparation is not a fix or integration decision for this failure.

If the coordinator authorizes a separate future local diagnostic after timing ends:

- First implement a minimal Rust runner failure-path exporter for bounded Native `host.log`, original path, full size/hash and explicit truncation metadata, into REPORT_ROOT after failure snapshots/host stop and outside all timers. Keep environment/auth dumps, fixture bodies and payloads out of the archive. Preserve success flow, caps, pricing and retry behavior. Prepare only in a fresh isolated worktree at exact trigger 82b64319; focused cap/hash tests, fmt, Clippy and independent source review precede any integration after all four current outcomes are reviewed. No CI push/cloud run is authorized by this audit.
- Add Rust diagnostic timestamps and correlation IDs for connect, request headers, body write start/end and bytes, response status/header/body read, socket error kind and response write completion. Record frame wire/metadata/chunk sizes and a request digest without payload contents.
- Correlate application Worker entry/count, body read start/finish, DO dispatch start/finish and actual DO upload validation/PUT/CAS/retirement phases. Retain elapsed times and exact error text. Do not infer DO work from an outer request line.
- Use the existing Rust runner or a small Rust local harness against fresh workerd/R2/SQLite state, with the original failed fixture schedule and frozen limits. Run one controlled attempt sequentially, retain failure intact, and separately exercise lost replies without write retries. No MJS runner and no paid deployment.

Nothing in this proposal was executed. A new diagnostic build/workflow is separate evidence and must not change the active screen. Cost/performance remains INVALID/incomplete until every declared profile/provider is comparable and all guards/tails are reviewed. Final acceptance still requires five fresh original-R1/final pairs across all four profiles; screen data never supplies those pairs. Unknown Worker CPU, DO duration, monthly SQLite retention and Native compute keep the full bill null. No global optimum, plateau, or stopping claim follows.
