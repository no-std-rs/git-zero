# R17 evidence scope

The frozen source is `c128197cc2cf1761e93832c63eec4c25c1268249`, based on
approved R16 `603b224eda3719c7525db00ac6a112a71368d7b3`. This directory adds
documentation only. The [final independent source review](source-review.md)
approves the source and scoped local correctness evidence. Independent review
of this compact ledger and capsule remains required before integration.

The implementation keeps the public Worker gateway and local Rust Git codec.
The Rust codec can send bounded authenticated service RPCs over one Worker
socket. Each actual Upgrade attempt contributes one Worker request; subsequent
application messages contribute none. Each RPC calls the existing DO HTTP
bridge once and retains its ordinary DO receipt, R2/SQL operations, durability
and proof checks. There is no DO socket or DO socket billing discount. Rates,
scorer, workload objective and existing route limits remain unchanged.

The first exact production control sent two RPCs through one Upgrade: Worker
requests fell from two to one, while both paths performed two DO requests, one
40-byte PUT, one 40-byte GET, and 15/3 SQL rows read/written. This control starts
from fresh phase counters; observers retain their own actual scopes.

## Exact final controls

All times below are correctness-test runtimes. They are not latency samples,
CPU measurements or evidence of a speed improvement.

| Control | Result | Runtime |
| --- | --- | --- |
| Production topology, maximums, protocol and HTTP mock controls | 3 Rust tests pass; 21 production protocol cases | 44.01 s |
| Real Git, socket enabled | Pass | 76.64 s |
| Real Git, socket disabled | Pass | 45.28 s |
| Receiving/authentication/publication/reply faults | All 29 rows pass | 158.44 s |
| Library | 119 tests pass | 56.93 s |
| All-target Clippy | Pass | 8.05 s |

The production gate's 34-entry before/after maps match. Each new real-Git and
fault run has the same 37-entry before/after map, equal to current source,
configurations, lockfile and Native/runner binaries. The capsule contains the
exact tracked c128 source/build inputs and original maps. Packaging verification
rehashes the current mapped binaries; their ELF bytes remain local hash-only.

All eight paired pushes have equal physical DO/R2/SQL operations and history
certificate sizes. The first socket push pays its Upgrade; pushes two through
eight each save one Worker request. All six retained contract stages have equal
physical storage gauges and DO/R2/SQL totals; eleven actual public body files
are byte-identical. These are scoped operation counts, not an accepted net-cost
result. The separate projected-thin receive restores Projection to Full once:
socket/HTTP use 3/4 Worker requests, while both use 4 DO requests, 6 GETs reading
19,938,790 bytes, 3 deletes and 4/1 SQL rows read/written. Actual observer work
is included in these scopes.

The real Git controls exercise small and thin pushes, default large pushes,
fresh and seeded clients, latest and historical reads after retirement, empty
probes, projection recovery, strict fsck and archive equality. A normal fixture
checks SQLite against 24,576 bytes. The deliberately maximal metadata control
uses a larger SQLite state; it is not presented as the normal storage gauge.

## Bounds and recovery

The socket client uses pinned Tungstenite 0.30.0 with only its handshake feature.
The dependency audit records 20 added packages, preserved existing versions,
available MSRV declarations and ten inspected source hashes. Those source files
are in the capsule. This is not an independently run Rust 1.85 build claim.

One registration holds one authorized socket and one synchronous outstanding
RPC, rotating before a seventeenth RPC. Requests use at most 16 KiB RFC fragments;
responses use at most 16 KiB application messages bound to an exact RPC ID,
sequence, offset, declared total and terminal count, followed by an ACK.
There is no pending payload map, object cache or replay queue. The Worker API's
incoming messages are complete application messages, not exposed RFC frame
counts; Native telemetry separately records actual request frame attempts.

Actual maximum PUT and bulk-read controls use 16 MiB. Metadata remains limited
to 1,500,000 bytes and whole GZUP to 16,779,276 bytes. One byte over each boundary
returns Unsupported before application I/O. Generic legacy scalar GET chooses
HTTP before socket I/O and retains its 128 MiB cap. Its retained 128 MiB
compatibility proof uses a mock HTTP peer, not a workerd R2 legacy-object run.
The 429 pre-RPC mock pays one Upgrade before HTTP; 401 performs no application
PUT. Ambiguous handshakes and attempted RPCs never fall back or resend.

The 32 MiB joint retained cache/certificate budget charges fixed socket state
and a 262,144-byte transport reserve. Exact maximum controls retain an additional
16,230 requested bytes after PUT and 32,614 after GET and output drop. Their
50,501,711-byte transient live requested-allocation peak includes caller fixture
and request-local copies. These allocator readings are not RSS or the total
memory of a Worker isolate. ACK gating bounds one response per registration;
many-repository and hostile-authenticated-peer memory capacity are unmeasured.
Thirty-four RPCs use three paid Upgrades, two rotations, 1,091 request RFC frames,
1,125 response messages and 34 ACKs. Maximum-error ownership and the next
distinct reconnect are also retained.

The five post-CAS faults are lost reply, bad ID, duplicate segment, missing
terminal and malformed terminal. Each occurs after one actual upload PUT and
one CAS, returns a logical 503 with its 22-byte body accounted, drops the socket
and performs no replay. The same Native's next distinct operation pays one fresh
Upgrade and physically lists `/_services/gc` before its next PUT. The driver
checks strict fsck and archive equality after recovery.

The first private unknown-SQL row changes known rows to NULL while accounting
remains complete. The missing-receipt row then makes completeness false.
Later body/socket rows inherit that sticky NULL/incomplete state; they do not
prove fresh per-case transitions. Actual known operations and original traces
remain recorded. A reconnect or metrics read never converts unknown cost to zero.

Registration identity, authentication, framing, route limits, ACKs, session
rotation and stale-session dispatch are covered by the 21 production controls.
Passive cross-request socket close is best effort in workerd. The final control
sends an actual old-session PUT after replacement registration and proves zero
DO/R2/SQL side effects; safety does not depend on passive close timing.

Old Native compatibility and warm hosting are supported by structural common
HTTP-path comparison and independent capability negotiation, plus exact-final
small HTTP, absent-capability/mock and socket-off Git controls. No final-source
old-Native binary or warm-host Git run is claimed.

## Historical evidence and failures

The original 479-file handoff, provisional review `ee7420bf...`, original source
maps and all failed attempts remain immutable in the capsule. Earlier Git map
`d23b1...` and receiving map `2ff956...` have no retained byte-exact historical
source snapshot. The historical continuity JSON predates final socket telemetry;
it is not a complete final-source diff. Their controls remain historical. The
new unchanged exact-c128 runs close the required behavior evidence gap.

| Attempt | Retained outcome and correction |
| --- | --- |
| Initial sandbox library run | 97 pass/21 fail: 20 loopback EPERM and one owning-size assertion. Only truncated original tool output exists; no full log or earlier source capsule is invented. Corrected authorized runs pass 118, then final 119. |
| Initial test builds and lint | Wrong test API, unwrap and lint failures; original logs retained. No runtime control occurred for build-only failures. |
| First-proof-02 | Legacy mock passed; shared artifact-directory collision stopped production before workerd started. |
| Initial receiving matrix | Sixteen rows retained before an inherited volatile test fault arm disappeared across an actual DO eviction. The push succeeded because the fault did not fire. Test-only nonce/trace continuity was corrected; no production receipt counters were substituted. |
| First-proof-final-01 | Twenty protocol controls passed, then passive old-session close timed out. Original log and timeout remain. The final actual old-session PUT control proves dispatch rejection. |

Earlier prototype, maximum, Git on/off and corrected 29-row controls are
retained with their own source maps and times. They do not become final-source
runs merely because later controls passed. Failure times carry no performance
or stopping significance.

## Publication and remaining work

[retained-artifacts.json.gz](retained-artifacts.json.gz) inventories every regular
file recursively in all twelve artifact roots, including generated real-Git
trees, wire responses, R2 blobs and SQLite/WAL/SHM state, plus relevant direct
logs, reviews, designs and current mapped ELF files. Every entry records path,
size, SHA-256, publication status and reason. The previously reviewed 479 files
and final 956 top-level files/233,677,080 bytes are independently matched during
packaging; they are scoped subsets, not the complete recursive file total.

The capsule publishes compact logs, source, JSON receipts/maps/reviews/raw proofs
and bounded wire bodies. Large wire payloads, generated Git fixture state,
workerd persisted state and ELF bytes stay local hash-only, explicitly listed.
Earlier overwritten binaries remain historical map identities; their old bytes
are not claimed retained. This is not a self-contained offline replay environment
or an off-machine backup of the large local artifacts.

Every capsule member is reopened, read and checked for byte size and SHA-256
against the exact member inventory. Duplicate and unsafe paths are rejected.
[verification.json](verification.json) records counts and the capsule hash;
`SHA256SUMS` covers published files. Packaging performs no service start, build,
runtime test, CI run, push, deployment or performance benchmark.

Worker CPU, DO duration, monthly SQLite retention, external Native compute and
the full bill remain UNKNOWN/NULL. Wall-clock test time is not CPU billing.
Prospective two-pair screens per optimization and the mandatory five fresh
original-R1/final pairs across all four profiles remain separate work. No net
cost/speed improvement, candidate promotion, plateau, stopping decision or
production deployment follows from this correctness ledger.
