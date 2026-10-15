# Roadmap

The goal is a Git HTTP client/server library with an allocation-free,
`no_std` core, stable synthetic repositories, on-demand packing, optional caching,
and host adapters for native `rustix`, Wasm, and Cloudflare Workers.

The current priority is **read-only HTTP server parity**, including its tests.
Complete the server milestones below before starting further client features.
Existing client code and regression tests remain in place. Read/write server
support follows in section 7; completed client work and its deferred fetch/push
follow-ups are recorded in section 8.

Server parity means real Git can discover refs, clone, and fetch from git-zero
over smart and dumb HTTP with the same refs, reachable objects, checkout contents,
and supported negotiation behavior as Git's reference server. The target includes
protocol v0/v1/v2, SHA-1/SHA-256, and shallow/filter fetches within explicit resource
limits. Each capability stays unadvertised until its implementation and tests
pass. Exceeding a limit must produce a documented failure without a successful
partial response. Byte-identical pack encoding is not an acceptance requirement.

Real Git is the client and Git `http-backend` or a static bare repository is the
oracle for server acceptance. git-zero's client must not be a prerequisite for
proving server correctness. HTTP authentication and deployment policy belong to
hosts. Write support (`receive-pack`) has its own implementation and acceptance
milestone after the read-only server gate.

## 0. Working foundation

- [x] Rust workspace with `std`/`alloc` disabled in core dependencies.
- [x] Borrowed objects, SHA-1 IDs, bounded writers, and explicit error handling.
- [x] Root-level snapshot files, sorted tree entries, deduplicated blobs, and a
  deterministic root commit without history.
- [x] Smart HTTP v0 discovery/upload-pack and dumb HTTP refs/loose objects.
- [x] Stream version-2 packs with stored DEFLATE blocks and SHA-1 trailers.
- [x] Pluggable complete-pack cache interface and uncached operation.
- [x] Client ref discovery, smart pack download/checksum validation, dumb loose
  object fetch, and transport interface with caller-owned response buffers.
- [x] Shared shallow/multi-tip fetch encoding, borrowed commit/tree/tag views,
  and bounded complete pack materialization with a caller-owned ID index.
- [x] Native `rustix` libraries and local CLI examples for both roles.
- [x] Allocator-free Wasm build and stateless Cloudflare metadata snapshot demo.
- [x] Parallel real-Git interoperability matrix and Wasm clone tests.
- [x] README, testing instructions, and CI checks for native/Wasm/bare-metal builds.

## 1. Server objects, repositories, and refs

- [x] Nested trees, executable files, symlinks, empty trees, and configurable refs.
- [x] Caller-owned object metadata/index arrays to avoid repeated scans and hashing.
- [x] Bounded readers for large or generated objects; explicitly size scratch and
  resumable state, and avoid requiring a whole object in one borrowed slice.
- [x] Incremental root-entry snapshot builder with configurable commit identity/message/time.
- [x] Validation of custom repository object graphs, tree order, and reachability.
- [x] Native file-backed object provider using `rustix`, with optional prepacked data.
- [x] Empty/bare repositories, unborn and detached HEAD, symbolic refs, independent
  branch tips, lightweight/annotated tags, and peeled ref advertisements.
  Caller-owned refs carry independent IDs, raw symbolic targets and validated
  final peeled IDs. Real Git discovery, clone and fetch compare native smart/dumb
  hosts with Git's HTTP backend and static bare repositories across the Git/platform
  CI matrix, including unborn HEAD with other refs and detached HEAD beyond them.
- [x] Serve the correct reachable closure for selected refs and tag targets,
  including merge histories and repositories with objects beyond HEAD's closure.
  Per-request wants, traversal/index arrays, scratch and a graph-read byte limit
  bound selection without allocation. Matched no-capability upload requests compare
  exact imported objects with Git. Legacy HEAD-key caches and whole-pack hooks reject
  independent roots; requested-closure cache keys and prepacked reuse remain in section 3.
- [x] SHA-256 object IDs, repository validation, ref advertisements, packs, and
  object-format negotiation alongside the existing SHA-1 path.
  Format-tagged IDs, fixed hash state and width-aware graph/packet limits remain
  allocation-free. Native smart/dumb Git comparisons cover both formats, empty
  and unborn remotes, symbolic/detached HEAD, independent refs, tags and selected
  merge closures; Git's strict importer also verifies SHA-256 snapshots.

Acceptance: Git `fsck --strict` accepts nested and generated snapshots; deterministic
inputs yield identical IDs on native and Wasm without enabling an allocator.
Real Git clone/fetch tests compare every advertised ref and reachable object's
ID, kind, body, file mode, and checkout content with independently generated Git
repositories. Cover empty/unborn remotes, detached HEAD, distinct branches,
annotated tags, deep trees, merge histories, binary blobs, and both object formats.

## 2. Smart HTTP and upload-pack parity

- [x] Multiple wanted refs/IDs and bounded reachable-object selection, with an
  explicit policy for unadvertised, unreachable, and arbitrary-object wants.
  Only advertised HEAD, ref and peeled IDs are accepted. Unknown and merely
  reachable wants fail before output; validation rejects unreachable provider
  objects. Duplicate/overlapping wants share objects. Known commit haves now prune
  complete common history; include-tag remains unadvertised.
- [x] Broader v0 capability negotiation, ACK/have negotiation, sideband and error packets.
  Single ACK, multi_ack and multi_ack_detailed rounds, both sideband modes,
  no-progress and host-written terminal ERR packets have allocation-free tests.
  Stateless requests replay wants/common haves; selected packs omit their closure
  within one shared graph-read budget. Native Git-oracle comparisons check exact
  incremental objects and imports in SHA-1/SHA-256. Wasm tests check framing,
  ACKs, checksums and fixed memory; complete-pack hooks may resend common history.
- [x] Protocol v1/v2 (`ls-refs`, `fetch`) with accurate capability advertisements.
  Header-selected v1 discovery and stateless v2 ls-refs/fetch support symrefs,
  peels, unborn HEAD, prefix filters, ACK rounds, include-tag and sideband packs.
  Native real-Git comparisons cover SHA-1/SHA-256, all HEAD states, exact pack
  membership and incremental fetch; Worker Git clones cover v0/v1/v2 and its
  fixed-memory ABI checks both formats. Bounded shallow/blob-filter support is
  recorded below; other optional extensions stay absent.
- [x] Incremental requests and responses, backpressure, cancellation, and resume points.
  Caller-owned bounded request intake waits for complete framing before command
  parsing. Pull cursors retain pack/hash/sideband state without response-sized
  storage; hosts stage unsent bytes and resume when writable. Terminal cancellation
  prevents further reads or success terminators. Native close-delimited streaming
  passes the Git-oracle matrix in both formats; Worker pulls drive streamed
  responses. HTTP chunked request decoding and nonblocking socket polling remain
  adapter work in section 4.
- [ ] Shallow/filter requests with bounded traversal and documented rejection paths.
  Absolute depth, shallow clients, absolute deepening/unshallow, blob:none and
  blob:limit now share caller-owned traversal arrays and one graph-read budget.
  Native Git-oracle clones compare boundaries and delivered objects for v0/v1/v2
  in SHA-1/SHA-256, including merges and combined depth/filter requests. Worker
  clones and repeated fragmented/cancelled ABI pulls retain the 24-page ceiling.
  Relative/time/exclusion deepening and additional filters remain rejected.
  Partial clones use --no-checkout: lazy promised-object wants still need an
  explicit extension of the advertised-ID policy before general checkout works.
- [x] Proper method/content-type checks, service selection, protocol-version
  handling, route normalization, and a wider malformed-request corpus.
  Shared routing rejects wrong methods/media types, duplicate services/versions,
  unsafe path components and malformed escapes before output. Hosts pass HTTP
  metadata explicitly; native and Worker hosts decode bounded gzip requests.
  Negative, truncated-command, retry, output-guard and seeded fuzz cases cover
  the new surface. HTTP streaming/chunked framing remains in sections 2 and 4.

Acceptance: supported cases from upstream HTTP tests have mapped, reproducible
git-zero coverage; unsupported operations return explicit errors.
Use real Git to exercise v0/v1/v2 discovery, clone, incremental fetch, multiple
wants, have/ACK negotiation, sideband, shallow/deepen, and advertised filters.
Compare refs and imported object closures with Git's server; retain packet/HTTP
traces. Test malformed packets, unsupported capabilities, unauthorized wants,
wrong methods/media types, truncated bodies, and exhausted request/output budgets.

## 3. Dumb HTTP, packing, and cache backends

- [ ] Serve `objects/info/packs` and valid pack/index downloads for packed dumb
  repositories, alongside loose objects and consistently published refs/HEAD.
- [ ] Bounded actual DEFLATE compression, with stored blocks as a selectable fallback.
- [ ] Reuse prepacked entries and existing packs when their closure matches the request.
- [ ] Optional delta generation with strict CPU/memory budgets.
- [x] Stream pack index creation alongside packs for dumb HTTP reuse.
  Caller-owned entry slots collect IDs, wire CRCs and 64-bit offsets during one
  stored-block encoding pass; the sorted version-2 index streams after the pack.
  SHA-1/SHA-256 indexes match Git's independent indexer byte for byte, without
  allocation or pack-sized staging. HTTP publication remains the item above.
- [ ] Versioned keys incorporating object format, encoder choices, and requested closure.
- [ ] Fixed-memory example cache, native disk cache, and async KV/R2 host adapters.
- [ ] Cache integrity validation, eviction, cold-miss coordination, and observability.

Acceptance: cache miss/hit/reuse paths produce equivalent objects; cache failures,
corrupt entries, and concurrent writers have tested behavior.
Real Git must clone and fetch loose-only, packed-only, and mixed dumb repositories.
Validate generated packs/indexes with Git's strict importer and object checks;
compare compressed, stored-block, prepacked, cached, and uncached results. Cover
corrupt/truncated cache entries, changing refs, eviction, failed writes, and
concurrent cold misses. Optional delta generation can follow the correctness
baseline without blocking server parity.

## 4. Server adapters and hosting

- [ ] Native IPv6/listener and TLS integration, HTTP/1.1 request/response framing,
  chunked transfer, keep-alive, timeouts, and configurable connection/body limits.
- [ ] Poll-based `rustix` server with independent per-connection caller-owned state.
- [x] Streaming Wasm server ABI and host I/O callbacks without Rust allocation.
  The start/read/cancel ABI pins one immutable snapshot per instance and streams
  into bounded host staging through ReadableStream pull/cancel callbacks. Input
  remains bounded and complete before prepare. Tests cover fragmented output,
  cancellation, restart, independent instances, both formats and fixed memory;
  real-Git Worker clones cover v0/v1/v2 and dumb HTTP.
- [x] Worker integration with async storage, streaming responses, and isolate-safe reuse.
  Optional KV/R2 snapshot bindings await publication, bound streamed reads and
  authenticate immutable records before preparing independently owned Wasm
  responses. Cross-isolate fixtures cover concurrency, cancellation, eviction,
  corrupt/oversized entries and failed storage; real Git clones/fetches both
  adapters over smart and dumb HTTP. Complete-pack storage adapters remain in
  section 3; these bindings persist metadata snapshots.
- [x] Replace long embedded snapshot URLs with optional immutable persisted snapshots;
  preserve stable IDs across discovery and downloads, retries, and isolate changes.
  Versioned SHA-256 URLs cover file contents and expiry; missing/expired/corrupt
  records fail explicitly and never recapture current metadata. Configurable
  retention, KV propagation limitations and R2 physical cleanup are documented.
- [ ] Tree-shaped Cloudflare metadata and public binding projection, with deployment
  authentication and explicit public variable selection.
- [ ] Configurable retention, diagnostics, and CPU/memory measurements for Workers.

Acceptance: native and Worker hosts share protocol behavior; overlapping requests
cannot overwrite each other's buffers or change repository identities.
Run real Git clone/fetch against both adapters and compare objects/refs with the
reference server. Exercise short reads/writes, slow and disconnected clients,
chunked requests, concurrent sessions, timeouts, cancellation, storage failures,
and repeated Worker requests. Define snapshot consistency across discovery and
upload-pack, and verify bounded per-connection state and isolate-safe reuse.

## 5. Server parity tests and resource budgets

Tests accompany each server milestone above. Existing git-zero client tests
remain regression coverage; server completion is measured with real Git clients.

- [x] Map upstream `t5550`/`t5551` cases to supported/planned/out-of-scope features;
  see `tests/upstream-http.md` for the pinned reference and independent comparisons.
- [x] Extend that map into a server acceptance matrix with a named local case,
  reference-server comparison, host/protocol/object-format coverage, and explicit
  remaining gap for every in-scope feature. Separate server results from client
  limitations; a Git-only reference baseline does not prove git-zero server parity.
  `tests/server-acceptance.md` records native/Worker, protocol and format coverage,
  independent oracle evidence and explicit unimplemented or untested gaps.
- [ ] Reusable upstream fixture adapter where Git's harness allows replacing a server;
  record GPL provenance for any copied tests rather than copying them silently.
  `tests/server_fixture.py` now exports any Git-created fixture's reachable raw
  objects, refs and HEAD for the native file-provider harness, including packed
  sources and both object formats. This independently authored adapter does not
  substitute into Git's Apache-specific upstream harness; that integration remains.
- [ ] Empty/bare repositories, arbitrary refs, tags, large binary blobs, deep histories,
  nested trees, aggressive packing, and deltas served by git-zero in the real-Git
  client interoperability matrix, including protocol versions and object formats.
  Native `references` now compares 40-level trees, 32 changing historical blobs,
  a merge, a 384 KiB binary, executables, symlinks, unusual refs and nested tags
  over v0/v1/v2 and dumb HTTP in SHA-1/SHA-256, with full and incremental clones.
  The Git oracle is packed-only and must contain deltas; git-zero serves the same
  exported raw objects. Worker rich fixtures and server compressed/delta pack
  generation/reuse remain gaps rather than inferred coverage.
- [ ] Incremental fetch after changing a backend snapshot, pack reuse and eviction races.
  Native smart/dumb tests now fetch file-backed snapshots through A → B → A at
  one URL, validate refs, object bodies and checkouts with real Git, and exercise
  fixed-memory complete-pack hits and eviction without allocation. Core tests
  cover cache/backend/output failures and bounded retries. Concurrent eviction
  and writer coordination remain host-backend work.
- [ ] Fuzz packet lines, HTTP routing, zlib/pack parsers, and object-graph validation.
  Seven bounded libFuzzer targets now cover packet lines, client discovery/HEAD/
  pack framing, HTTP routing/upload requests, commit/tree/tag graph validation,
  selected closures and
  incremental zlib inflation with expansion budgets, loose-object envelopes and
  pack entries/delta instructions.
  Their shared assertions and seeded mutations also run in ordinary Cargo tests;
  CI runs AddressSanitizer smoke campaigns. Pack-index parsing needs coverage
  when its client API is implemented.
- [ ] Expand server fuzz coverage for capability/want/have negotiation, v2 commands,
  shallow/filter state, incremental HTTP framing, and concurrent/cancelled request
  state. Retain regressions and check bounded work, state reset, and output guards.
  V0 capability conflicts, ACK/have phases and sideband responses and v2 command
  framing, ls-refs/fetch, unsupported extensions and HTTP gzip envelopes have
  seeded mutations. Shallow/filter mutations now exercise selected synchronous
  and pull responses, boundary framing, budgets and cancellation; concurrent
  host state remains planned.
- [x] Allocation tracking for native file providers, repository validation, prepacked
  verification, and pack generation/serving.
- [ ] Fixed-budget benchmarks and Wasm memory growth checks.
  `tests/benchmark.py` uses a configurable synthetic Git repository and identical
  file-provider export through a shared Rust HTTP host. Paired discovery/clone/fetch
  measurements retain client and server wall/CPU/RSS, child usage, response bytes,
  startup costs and buffer budgets. Encoded files share a sendfile path with bounded
  fallbacks; optional Rust loose caching charges preparation to startup. Wasm and
  simultaneous process-tree memory remain explicit future work.
  Git 2.53.0 is pinned for timing comparisons. Explicit matched/default/custom
  compression and delta policies identify equivalent server features. Two local
  object-read workloads now compare Git and the new decoder against identical
  loose/packed inputs, verify complete object output and record actual pack
  deltas. Pinned CI covers matched and default Git policies; additional server
  compression/delta encoders remain planned in section 3.
  Local decoder throughput now uses budgeted bulk stored/back-reference copies,
  small Huffman prefix tables, batched Adler-32 and slicing-by-eight pack CRCs.
  The native read host allocates exact entry sizes and delta-only reconstruction
  slots, indexes bases and buffers batch output. Checksums, canonical IDs,
  chunk-independent failure positions and allocation-free core decoding remain
  covered. Reports now separate user/system CPU, voluntary/involuntary context
  switches and minor/major page faults. The paired measurements and remaining
  authenticated pack-read overhead are recorded in `tests/README.md`; controlled
  cold-cache tests and native mapped/indexed object storage remain performance
  work. Measurements are not a server parity/performance gate.
  Worker ABI tests now enforce a 24-page initial linear-memory ceiling, fixed
  input/output capacities and no growth across repeated, full-capacity and invalid
  requests. Explicit opt-in wall/CPU/RSS limits now check the largest raw
  benchmark observations, retain failures in schema-3 reports, and enforce native
  memory caps in CI. JavaScript/isolate memory accounting and simultaneous process
  tree RSS remain future work.
- [ ] Opt-in local Linux kernel repository benchmark using a caller-provided clone
  and pinned revision; compare supported clone/fetch and object-reading workloads
  with real Git over loopback, recording wall/CPU time, peak RSS, throughput,
  and caller-owned buffer budgets for cold/warm runs.
  Pinned caller-repository fixtures now feed eight paired HTTP and two local
  object-read workloads, with bounded object/body loading and source-preservation
  tests. Kernel-scale preparation and controlled
  cold-cache runs remain future work.
- [x] Protocol traces and retained artifacts on failure; CI matrix over Git versions
  and Linux/macOS, with jobs isolated by temporary paths and ephemeral ports.

Acceptance: each server feature has positive and negative tests using independent
fixtures and the reference server. Native/Wasm allocation and memory ceilings,
concurrency isolation, and malformed-input campaigns are enforced in CI. Benchmarks
compare real Git clients against Git and git-zero servers on identical repositories,
including cold/warm clone and incremental fetch; retain wall/CPU/RSS, wire bytes,
throughput, startup costs, and caller-owned buffer budgets. Keep optional
kernel-scale benchmarks separate from mandatory, reproducible correctness checks.

## 6. Server parity completion gate

- [ ] All in-scope server cases have passing real-Git comparisons on native and
  Worker/Wasm hosts across the supported Git/platform CI matrix.
- [ ] Smart v0/v1/v2 and dumb HTTP clone/fetch, refs/tags/HEAD, incremental and
  shallow/filter fetches, and SHA-1/SHA-256 pass their acceptance tests.
- [ ] Streaming, concurrent requests, cancellation, cache/storage failures, and
  resource-limit failures preserve repository consistency and bounded memory.
- [ ] Server documentation accurately lists capabilities, host responsibilities,
  configured limits, and remaining optional work; no advertised feature lacks tests.

Start further client development after this gate passes. The next implementation
step is completing shallow/partial-fetch follow-ups (relative/time/exclusion
deepening and promised-object retrieval), then packed dumb HTTP publication and
the remaining native/Worker hosting work.

## 7. Read/write server support

- [ ] A host-owned writable object store and ref transaction interface, with
  explicit durability, locking, and compare-and-swap semantics for old/new IDs.
- [ ] `receive-pack` discovery, bounded update-command and capability parsing,
  pack ingestion, and accurate status/error responses.
- [ ] Quarantine incoming objects until pack checksums, object IDs, delta bases,
  graph connectivity, and requested ref updates have been validated. Bound
  request size, object count, expanded bytes, delta depth, and validation work.
- [ ] Branch/tag creation, update, and deletion; configurable fast-forward/force
  policy and host authorization before publishing objects or changing refs.
- [ ] Advertise atomic updates only when the host can commit the complete ref
  transaction atomically. Failed validation or rejected atomic pushes publish
  no ref changes; retries and concurrent pushes preserve old-ID checks.
- [ ] Real Git push tests against git-zero and Git's reference server: initial
  and incremental pushes, annotated tags, deletion, non-fast-forward rejection,
  allowed force pushes, stale old IDs, multiple refs, and atomic failures.
- [ ] Negative and resource tests for corrupt/truncated packs, missing bases,
  disconnected objects, denied updates, storage failures, concurrent writers,
  interrupted requests, and exhausted caller-owned buffers.

Acceptance: real Git can push, then clone/fetch and pass `fsck --strict` against
the accepted repository. Rejected pushes leave the promised ref/object visibility
unchanged. Native and Worker hosts document writable storage, authentication,
durability, and transaction guarantees before enabling receive-pack.

## 8. Client work — deferred until server parity

- [x] Incremental zlib inflation into caller buffers with output/ratio limits.
  The core decoder handles stored/fixed/dynamic DEFLATE, validates Adler-32,
  borrows a 32 KiB history window and uses less than 2 KiB of resumable state.
  Output and prefix expansion budgets are enforced before each emitted byte;
  callers explicitly choose a prefix allowance. Python zlib, real Git loose
  objects, allocation tracking and a bounded fuzz target cover the decoder.
- [x] Validate loose-object headers, lengths, and IDs before exposing contents.
  `Object::from_loose`, `client::decode_loose` and `fetch_loose_object` require a
  canonical header, exact body size and matching SHA-1 before returning a body.
  Complete zlib validation and rejection of trailing bytes precede exposure.
- [x] Pack entry reader, CRC checks, object IDs, and bounded delta resolution.
  The SHA-1 pack reader checks container framing/checksum, entry lengths and
  exact count/closure, computes wire CRCs for comparison with index CRCs, and
  reconstructs offset/reference deltas against explicitly supplied bases.
  Canonical IDs, same-pack offset identity and output/instruction/depth budgets
  are checked; base lookup and whole-fetch storage remain host responsibilities.
- [ ] Caller-owned pack indexes and object sinks for complete smart fetches.
- [ ] Dumb object-graph traversal, `objects/info/packs` and pack/index downloads,
  with an explicit memory bound for visited objects and traversal state.
- [ ] Correct empty/unborn remotes, symbolic refs, branch/tag selection, and SHA-256.
- [ ] A complete clone/materialization example using a host object sink and checkout.
- [ ] Client capability and ACK/have negotiation, protocol v1/v2, sideband/error
  decoding, shallow/filter requests, and bounded incremental transport state.
- [ ] Client push: receive-pack discovery, ref selection, old/new-ID commands,
  bounded outgoing pack generation, capability negotiation, and per-ref status
  handling. Support creation/update/deletion and explicitly requested force or
  atomic operations within the server's advertised capabilities.
- [ ] Fetch/push interoperability against both Git and git-zero servers, with
  corrupt transfers, rejected refs, interrupted I/O, and exact buffer limits.
- [ ] Native client DNS/IPv6/TLS, HTTP/1.1/chunked framing, redirects, content-type
  validation, and configurable transport limits.
- [ ] Streaming Wasm client ABI and transport callbacks without Rust allocation.
- [ ] GitHub Pages demo at `no-std-rs/git-zero`: run the Wasm client in the browser,
  fetch this project's own repository from GitHub, resolve the default branch's
  tip ref, check out that commit in memory, and list every file in its tree.
  Plan browser CORS support or an optional HTTP proxy so the demo exercises the
  Git client rather than substituting a GitHub API file listing.
- [ ] Optional `git://` transport after HTTP server and client compatibility.

Acceptance: git-zero imports and checks out a real Git repository through both
HTTP protocols, including packed dumb repositories and delta-compressed packs.
Its push client publishes supported ref updates to Git and git-zero servers;
real Git independently verifies the resulting refs, objects, and checkouts.
Client-specific upstream coverage, parser fuzzing, and host transport tests expand
with those deferred features.
