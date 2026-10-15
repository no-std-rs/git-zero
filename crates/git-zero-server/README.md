# git-zero-server

Allocation-free Git smart and dumb HTTP server routing. Safe `no_std` Rust without `alloc`.

Smart HTTP discovery/upload-pack and dumb HTTP refs/loose-object routing.
The host supplies repository data, response output and HTTP framing.
Depends on `git-zero-core` with only `write` enabled.

Part of [git-zero](https://github.com/no-std-rs/git-zero).

Enable `read` for `SelectionStorage` and `serve_with_selection`, which select
bounded reachable packs from a validated repository with independent refs.

`SelectionBuffers<WANTS, OBJECTS, READ, PARSE>` optionally owns those fixed arrays.
Construction needs no allocator, and `storage(graph_read_limit)` lends a request
view. Capacity and storage-byte queries expose its host memory budget. Hosts can
still supply their own arrays through `SelectionStorage`.

V0 supports single ACK, `multi_ack`, `multi_ack_detailed`, `side-band`,
`side-band-64k` and `no-progress` in SHA-1/SHA-256. Requests replay wants and
common commit haves each round. Selection excludes their complete reachable
history within one graph-read budget; complete-pack hooks can resend it.
The hook's `PackOutput` sink frames ordinary pack bytes with a fixed 1 KiB buffer.
No progress messages or `ready`/`no-done` shortcuts are emitted.

Parse and selection failures precede output. Any later error invalidates the
whole response. Buffered hosts may replace it with `write_upload_error` on an
empty sink; streaming hosts close failed transfers.

`Request::git_protocol` selects v0 (absent), v1 or v2. V1 adds its version packet
before the v0 refs. V2 advertises `ls-refs=unborn`, `fetch`, agent and object
format. Its commands require complete delimiter/flush framing. ls-refs supports
symrefs, peels, unborn HEAD and multiple ref-prefix filters. Fetch supports wants,
haves, no-progress, thin-pack/ofs-delta hints and include-tag; full non-delta packs
satisfy the hints. Done requests omit ACKs and send a sideband packfile section;
other rounds return ACKs or NAK and wait for done. Selection includes annotated
and nested tags only when their final target is being sent, sharing one graph
budget. Whole-pack hooks retain their documented HEAD-only restriction.
Selection APIs advertise `shallow deepen-relative filter` (v0/v1) or `fetch=shallow filter` (v2).
They support absolute and relative `deepen` depths, shallow/unshallow boundaries, blob:none
and blob:limit filters, sharing the graph budget and object/index arrays.
Relative depth extends every client boundary reachable from the advertised
repository, including other branches, matching Git 2.53. Depth requests retransmit
their bounded closure; ordinary shallow haves stop
common traversal at the client's boundary. Root commits at the depth limit stay
shallow, matching Git. Time/exclusion deepening and other filters are
rejected. The advertised-ID want policy still requires partial clones to use
`--no-checkout`; lazy promised-object retrieval is deferred. Legacy complete-pack
APIs omit these capabilities and reject extended requests before output.
Ready shortcuts and other optional v2 features stay unadvertised.
V2 preflights every selected ls-refs line, including its attributes, against the
65,520-byte packet limit. Oversized attribute combinations return `LimitExceeded`
before output; hosts can report a resource error. A narrower ref-prefix or a
request without those attributes can still serve the same valid repository.

Hosts populate `content_type` and `git_protocol`. Uploads require
`application/x-git-upload-pack-request` (415 otherwise). Known routes reject wrong
methods (405), unavailable services (403), duplicate service/version fields,
malformed escapes, encoded separators, dot components and control bytes. Paths
accept zero or one leading slash; refs decode reserved bytes exactly once.
Service parameter names must be literal. Hosts own HTTP framing, content decoding,
authentication and request/output capacities. Native and Worker examples bound
both compressed and decompressed gzip bodies.

`stream::RequestBody` accepts bounded fragments into caller-owned storage and
checks declared length at EOF. Parsing still requires the complete bounded body;
HTTP headers, gzip and chunked transfer decoding belong to the host.
`stream::ResponseEncoder::new` prepares HEAD-only requests; `with_selection`
prepares exact requested closures with the existing graph budgets. Preparation
returns HTTP metadata after route/command/selection checks. Discovery, refs and
ACKs must fit the caller's control buffer; pack and loose bodies use inline state
and the slice supplied to each `read`. Keep the repository stable through EOF.

A successful `read` advances the cursor. Stage its bytes until the transport has
accepted them, including across short writes, then pull again. Pausing makes no
object reads; each pull performs at most one provider-body read. Raw reads are
bounded by output capacity; sideband uses at most 1 KiB of internal staging.
`cancel` is terminal and suppresses further reads and success terminators. A
backend read can be retried with the same stable provider; failed-call bytes are
invalid. Once HTTP headers are sent, unrecoverable errors require aborting the
transport. These cursors do not implement HTTP range requests or serialized
cross-process checkpoints. Separate cursors and buffers permit overlapping work.
