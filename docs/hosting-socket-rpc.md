# Bounded Worker socket RPC

The local hosting experiment can carry Native storage callbacks over an
opt-in authenticated Worker WebSocket. Every logical RPC still dispatches one
ordinary Durable Object HTTP request and keeps the same R2, SQLite, durability
and retirement-proof behavior. Public Git authority checks and gateway first
reads remain unchanged. Native Git still runs in a separate local Rust process.

The `repository-host --cloudflare` CLI requests this capability by default;
`--socket-rpc off` selects ordinary service HTTP using the same registration,
fixture and public Git flow. Older Workers and older Native clients retain HTTP.
Warm hosts can use sockets independently of gateway first-read capabilities.

The Worker counts the actual Upgrade as one service request in its triggering
phase. Socket application messages add no inbound Worker request charge; each
DO dispatch retains its normal request and real operation receipt. Registration,
unregistration and statistics stay HTTP. All failed Upgrades, fallback calls,
observer work and reconstruction remain in their actual physical scopes.
Transport diagnostics are separate from billed usage counters.

The transport currently covers bounded bulk reads, immutable PUTs and fused
upload/publication/retirement. Generic scalar GETs remain HTTP before I/O,
including the existing 128-MiB legacy response limit. A known unsupported or
capacity-limited Upgrade can select HTTP before an application RPC; failed auth,
ambiguous handshakes or attempted RPCs cannot silently retry or switch transport.

A socket is bound to the exact registration, repository and acknowledged
session. One synchronous SDK mutex permits one outstanding RPC. Exact binary
frames validate IDs, routes, phase, lengths and optional proof metadata before
binding I/O. Registration replacement/unregistration invalidates the old socket;
passive close is best effort, and an old-session RPC is rejected before DO I/O.
Sessions rotate at a pre-I/O boundary after at most 16 RPCs; every new Upgrade
is counted. No storage freshness check becomes a session lease.

Rust uses pinned blocking Tungstenite with handshake support, without an async
runtime or TLS dependency for the current loopback-only bridge. Requests form
one bounded application message using <=16-KiB RFC fragments; the library owns
masking, control frames and protocol parsing. Replies use <=16-KiB application
segments with exact RPC ID, sequence, offset, total and terminal count. Native
consumes the complete reply before acknowledging the Worker's response gate.
No payload queue, pending map or object cache is added.

Small frames avoid library buffers retaining full pack-sized capacities after
write/drain or shared input views. The existing joint 32-MiB cache/metadata
budget charges fixed socket state and a conservative 256-KiB retained transport
reservation. Request-local framing/response buffers remain transient; this is
not a total process or Worker-isolate RSS claim. Existing history certificates,
256 cache-entry limit and storage caps remain.

The whole fused GZUP body still fits **16,779,276 bytes**, including its header
and metadata; metadata separately remains <=1,500,000 bytes and chunks <=16 MiB.
A full chunk plus arbitrary maximum metadata is not a new supported fused body.
Unsupported shapes choose the existing split path before publication I/O.

Missing, malformed or lost replies after an attempted RPC are ambiguous. Native
drops the socket without resending it, accepts no unverified proof, and keeps
its existing dirty-state/generation-checked recovery before later mutation.
The Worker records each real DO receipt before proof generation/framing/send.
A lost committed reply cannot erase paid work; missing receipts remain explicitly
incomplete. A fresh connection cannot repair an accounting epoch reset.

Rust controls exercise production workerd topology, maximum valid requests,
legacy HTTP fallback, retained owning allocations, session rotation, exact
status/receipts, actual Git push/fetch correctness and post-publication faults.
Correctness controls are not a cost or latency benchmark. Fresh independent
screens and final validation decide measured improvement.

Worker CPU, DO duration, monthly SQLite retention and external Native compute
remain unknown, so the full bill remains NULL. Socket send buffering, concurrent
repositories in one isolate, runtime resets and production routing need separate
capacity evidence. No production deployment or standalone Worker Git codec is
implied by this local experiment.
