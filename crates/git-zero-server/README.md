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
Relative depth uses the nearest client boundary in wanted history, matching
Git 2.55. Unrelated branch boundaries remain intact; with no matching boundary,
the wanted history is complete (within the graph budget). Full clients keep
ordinary common-history pruning, including empty packs for no-op deepening.
V0/v1 negotiate relative depth on the first want; only v2 accepts its argument.
Without a depth, that negotiation leaves the ordinary fetch unchanged. Git 2.53 and
earlier extend all reachable client boundaries, so this case deliberately differs;
the versioned native oracle asserts both outcomes and identical imported objects.
Depth requests retransmit their bounded closure; ordinary shallow haves stop
common traversal at the client's boundary. Root commits at the depth limit stay
shallow, matching Git. Ordinary selection entrypoints reject time/exclusion
deepening; the separate revision API below enables it. The default advertised-ID want policy requires partial clones to use
`--no-checkout`. Explicitly authorized hosts can enable lazy promised-object
retrieval as described below. Legacy complete-pack
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


`WantOptions::all_visible_reachable(max_want_lines)` is an explicit host grant
for policy-aware selected responses. Use `serve_with_selection_policy` or
`ResponseEncoder::with_selection_policy` / `with_selection_storage_policy` after
routing/authentication and validation of the exact immutable public projection.
Old entry points pass `WantOptions::STRICT` and retain their bytes and denial
rules. Complete-pack/HEAD-only APIs do not grant this policy.

The grant permits any object kind in the validated union of visible HEAD/refs,
including unadvertised commits, trees, blobs and nested tags. Physical backing
objects outside that union are denied. This is stateless read authorization,
not proof of an earlier promise. Discovery and content/cache keys grant no
access: authorize every request before selection or cache lookup. Hosts retain
immutable generations for as long as their promised downloads should work;
changing a projection or collecting its objects can revoke previous wants.

V0/v1 advertise `allow-reachable-sha1-in-want` only with the explicit grant;
v2 uses its existing fetch command. Client packets, headers and queries cannot
turn it on. The exact opt-in discovery packets are preflighted before any output,
including unborn HEAD/first-ref capability carriers. The added 29 bytes can make
an old-limit ref fail with `LimitExceeded`; strict discovery limits are unchanged.

Every want occurrence, including duplicates, consumes the configured cap before
output. Exhaustion returns `LimitExceeded`; insufficient distinct selection
slots/scratch retain `BufferTooSmall`. Authorization uses the validated ID index
without body reads. Existing request bytes, object/ref/selection arrays and the
single structural graph-read budget remain separate finite limits. This is not
a whole-request logical-work budget: parsing, bounded metadata scans and ordinary
selection retain their existing accounting. A fresh request resets the counter.
Explicit blob roots survive `blob:none`, enabling ordinary partial-clone checkout.

For synchronous selected-pack caching, `serve_with_selected_pack` uses the same
route, want authorization and bounded selection as `serve_with_selection`.
Its trusted callback receives the exact selected indices, read scratch and a
`PackOutput` sink for this request. Call `cache::selected::write` using
`Selection::Indices(indices)` and that sink. The cache retains only verified raw
PACK bytes; ACKs, shallow updates, v2 sections and sideband framing are fresh.
ACK-only rounds, boundary probes and non-pack routes never call the hook;
an empty selected set still requires a valid empty pack. The old complete-pack
hook and its HEAD-only cache key are separate APIs.

Every cache hit still follows authorization and graph traversal. A cache or
backend error may follow provisional control output, so discard the whole output
on failure. Buffer the complete response before sending HTTP success metadata,
or abort an already started transport. A failed sink can leave a verified cache
entry for a later request. This synchronous hook retains no cache borrow or
response cursor; it adds no async storage or streaming-memory guarantee.

`serve_with_selected_pack_policy` accepts the same explicit `WantOptions` as
the policy-aware uncached API. Its shared preparation rechecks the grant and
want-occurrence cap before any callback/cache access, including a warmed hit.
The original selected-pack hook remains strict. Neither cached bytes nor a
client capability can enable a host policy.

The opt-in `receive` module implements receive-pack control framing,
advertisements and report-status/v2 replies without `std` or allocation.
`ControlDecoder` borrows one packet buffer and emits commands, shallow boundaries
and negotiated push options as input arrives; the pack suffix is never consumed.
All control bytes, command counts, shallow IDs and options have explicit budgets.
Raw ref names preserve non-UTF-8 Git bytes. SHA-1 and SHA-256 are supported.

The host advertises only capabilities it implements, authenticates the request,
stages commands/objects, resolves deltas and validates graph/ref updates before
publishing. `atomic` requires an authoritative all-ref transaction. `Report`
encodes the actual publication result, and `write_progress` can send sideband
messages beforehand without acknowledging success. HTTP framing and storage are
host operations. Existing read-only routing does not enable pushes implicitly.


### Time and exclusion deepening

Enable `read` and explicitly supply `RevisionStorage` to `serve_with_revisions`,
`ResponseEncoder::with_revisions`, or `with_revision_storage`. The convenience
`RevisionBuffers<WANTS, OBJECTS, READ, PARSE>` owns ordinary selection storage
plus exactly one state byte per object. `storage(graph_read_limit, limits)`
accepts `RevisionLimits { max_exclusions }`. Keep separate buffers
for overlapping requests. These APIs keep the strict advertised-ID want policy;
they do not enable promised-object wants or selected-pack cache hooks.

Only revision-enabled v0/v1 discovery advertises `deepen-since` and `deepen-not`.
V2 accepts these arguments under `fetch=shallow` on the same opt-in API. The
[Git protocol rules](https://git-scm.com/docs/gitprotocol-capabilities#_deepen_since)
allow their combination and repeated exclusions, but forbid combining them with
absolute depth. Timestamp cuts use committer times, include equality, and stop
old ancestry paths even when a later ancestor has a newer time. Exclusions remove
commit ancestry without removing shared trees/blobs needed by selected commits.
Names resolve against visible HEAD/refs only, using Git's ref-name expansions;
ambiguous names, raw IDs, revision expressions and missing refs fail. Annotated
and symbolic refs work. Tree/blob exclusions have no commit-ancestry effect.

Existing client boundaries produce independent shallow/unshallow changes,
including when a boundary body is absent from the pack. Newly unshallowed
commits seed missing parents. `blob:none` and `blob:limit` can combine with these
cuts. Revision requests with tree-depth or object-type filters currently return
`Unsupported`; ordinary selection still supports those filters.

Wire timestamps follow deterministic unsigned base-zero conversion, including
hex/octal/binary prefixes and signed unsigned conversion, with `u64::MAX` reserved
as an invalid sentinel. The last repeated `deepen-since` wins. Canonical Git
clients send decimal; acceptance of binary prefixes in reference Git depends on
its C library. Historical committer dates use Git's tolerant decimal-prefix rule.

The graph-byte limit covers every structural read and reread across exclusions,
date probing, boundary detection, common history, wanted closure and included
tags. Exclusion limits count duplicate wire arguments before ref lookup. The
caller bounds request bytes, want storage, object/index slots and read/parse
scratch. Metadata scans have the same object-capacity bound as ordinary selection
(quadratic worst case); ref resolution is bounded by the exclusion count and
visible ref names. This API does not implement a total request-preparation CPU
budget. Successful selection returns ascending provider indices; failures
invalidate all output. Stream preparation returns no successful response on
exhausted limits; failures after sending headers require transport abort.

Native tests compare real Git with `http-backend` for both hashes and v0/v1/v2.
The Worker example does not opt into revision storage yet.
