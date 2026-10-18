# Roadmap

Build a Git HTTP client/server library with an allocation-free `no_std` core,
stable repository identities, bounded packing/caching, and native and Wasm hosts.

The current priority is **read-only HTTP server parity**. Complete section 6
before further client features, kernel-client extensions or receive-pack work.
Existing client and kernel examples remain supported by regression tests.

Checked items describe merged implementation with local/CI evidence. An unchecked
item is a remaining deliverable, even when some prerequisites already work.
Experiments and unpublished branches do not count as shipped features.
The [server acceptance matrix](tests/server-acceptance.md) records named tests,
host/protocol/object-format coverage and gaps; [tests/README.md](tests/README.md)
owns test commands and detailed measurements.

Parity means real Git discovers refs, clones and fetches equivalent refs,
reachable objects, modes and checkout contents from git-zero and Git's reference
server. The target covers smart v0/v1/v2, dumb HTTP, SHA-1/SHA-256 and the bounded
shallow/filter features below. Git `http-backend` or a static bare repository is
the oracle; git-zero's client is not the acceptance oracle. Pack bytes need not
be identical. Advertise only implemented, tested capabilities. Reject exhausted
budgets explicitly; abort failed streamed responses without a success terminator.
Authentication, authorization and deployment policy belong to hosts.

## Next delivery order

1. Complete shallow fetch with time/exclusion deepening, retaining the real-Git
   checkout and history comparisons for explicitly authorized promised objects.
2. Publish packed dumb HTTP repositories and extend requested-closure caches to
   async hosts, retaining immutable refs/pack/index consistency.
3. Finish native header policy, request deadlines, HTTP framing and independent
   connections; add the Worker preparation/storage boundary and host diagnostics.
4. Close cross-host oracle, concurrency, fuzzing and resource-budget gaps, then
   evaluate the read-only completion gate. Compression and delta generation are
   optional performance extensions, not prerequisites for valid stored packs.

## 0. Working foundation

- [x] Safe `no_std` core without `alloc`, explicit errors and caller-owned buffers.
- [x] Separate core/client/server/native crates, feature checks and allocator-free
  Wasm/bare-metal builds; native CLI with a small `std` process wrapper.
- [x] Deterministic parentless snapshots, nested trees, incremental construction
  and configurable commit identities, times and messages.
- [x] Read-only smart and loose-object dumb HTTP, bounded readers and pack output.
- [x] Client discovery/fetch primitives, object decoding and checkout traversal.
- [x] Parallel Git interoperability tests, retained failure traces, package checks
  and CI over Linux/macOS and system/pinned Git versions.

## 1. Server objects, repositories, and refs

- [x] Nested/empty trees, regular/executable files, symlinks and configurable refs.
- [x] Caller-owned metadata/ID indexes, offset-based readers and resumable hashing
  for objects larger than scratch buffers.
- [x] Custom graph validation: hashes, tree order/links, parents, tag targets and
  exact reachability from HEAD and independent refs.
- [x] Empty repositories, unborn/detached/symbolic HEAD, independent branch tips,
  symbolic chains, lightweight/nested annotated tags and final peeled targets.
- [x] Multiple/duplicate/overlapping wants, exact selected closures and common
  history exclusion within caller-owned arrays and graph-read limits.
- [x] SHA-1/SHA-256 object IDs, refs, validation, packs and format negotiation.
- [x] Native immutable raw file providers and verified complete-HEAD prepacked
  responses. This does not open arbitrary Git loose/packed object directories.

Acceptance: independent Git fixtures and `fsck --strict` verify IDs, kinds,
bodies, modes, refs and checkouts. Native coverage includes merge histories,
deep trees and binary objects. Remaining Worker fixture coverage is in section 5;
host filesystem/name policies extend beyond core graph validation.

## 2. Smart HTTP and upload-pack parity

- [x] Protocol v0/v1 discovery/upload-pack and stateless v2 `ls-refs`/`fetch`,
  with format-aware capabilities, symrefs, peels and unborn HEAD.
- [x] Single/multi/detailed ACK negotiation, common-history pruning, sideband,
  no-progress and bounded host-written terminal ERR packets. V0 include-tag and
  ready/no-done shortcuts remain unadvertised; v2 include-tag is supported.
- [x] Bounded request accumulation and pull-based responses with backpressure,
  cancellation, independent buffers and resume points.
- [x] Absolute depth, client shallow boundaries, relative deepening and
  unshallowing, including merges and boundary updates in both formats.
  Relative depth follows Git 2.55 wanted-history semantics; versioned tests
  retain the Git 2.53 unrelated-branch boundary difference.
- [x] `blob:none` and `blob:limit` selection, including combined shallow/filter
  requests. Explicitly granted hosts also support ordinary lazy checkout.
- [x] Method/media-type/service/version checks, normalized routes and bounded
  gzip uploads, with malformed-input and output-guard regressions.
- [x] Promised-object retrieval for ordinary partial-clone checkout under an
  explicit host-granted immutable visible-closure policy. Strict defaults remain;
  hidden/dangling/unauthorized and mixed wants are rejected before output.
  Real-Git native/Worker cases cover v0/v1/v2, both formats and all object kinds;
  duplicate-counting want limits and policy-aware packet preflight are bounded.
- [ ] Time/exclusion deepening (`deepen-since`/`deepen-not`), including merges,
  combined options, malformed inputs, boundary reset and shared traversal budgets.
- [ ] Decide scope and rejection rules for further filters (tree/sparse/combine)
  and optional negotiation extensions before advertising them.

Acceptance: real Git discovery, clone, incremental fetch, shallow files and lazy
checkout match the reference server for supported cases. Compare exact selected
objects with matched requests; retain packet/HTTP traces. Negative cases cover
capability conflicts, unauthorized wants, truncation and exhausted budgets.

## 3. Dumb HTTP, packing, and cache backends

- [x] Loose-object dumb HTTP refs/HEAD and format-aware zlib object routes.
- [x] Stream stored-block packs and Git v2 indexes in one encoding pass using
  caller-owned entry metadata. Both formats match Git's independent indexer.
- [x] Versioned selected-pack keys for the exact unique object set, object
  format, stored codec and encoder revision, independent of provider order.
- [x] Integrity verification before hit output and miss insertion: framing,
  trailer/Adler-32, canonical IDs and exact unique closure, with byte/ID limits.
- [x] Caller-buffer `FixedCache` with disabled mode, eviction and counters.
- [x] Native `DiskCache`: bounded resident reads, exclusive staging, atomic
  complete-file replacement, eviction, counters and isolated readers.
- [x] Cooperative native cold-miss leases: nonblocking per-key attempts,
  recheck after acquisition, explicit unlock and cancellation/failure cleanup.
- [ ] Packed dumb publication: `objects/info/packs`, pack/index downloads and
  immutable publication consistent with refs/HEAD. Use actual pack checksums for
  Git filenames; selected-cache keys are a separate host identity.
- [ ] Reuse prepacked entries/packs for the requested closure, beyond the current
  encoder's complete-HEAD reuse; validate compressed/delta inputs and dependencies.
- [x] Synchronous selected-pack HTTP hook with authorization and selection before
  every lookup, fresh ACK/shallow/sideband framing and explicit failure semantics.
  Native memory/disk hosts pass real-Git miss/hit, A → B → A, exact physical-object,
  eviction/reopen and failed-output comparisons in both formats and v0/v1/v2.
- [x] Bounded native buffered example with an explicit private disk namespace,
  staging nonce and optional visible-closure grant. Warmed entries never bypass
  revoked permission or duplicate-counting want limits.
- [ ] Async complete-pack KV/R2 adapters and cold-miss coordination, separate from
  the implemented metadata snapshot persistence. Bound reads/writes, await complete
  publication and retain independent responses across failure, expiry and eviction.
- [ ] Production cache lifecycle integration for total disk quotas, crash/staging
  cleanup, directory durability and filesystem/distributed lock scope. The current
  synchronous example has one producer; cooperative leases remain an explicit API.
- [ ] Optional bounded DEFLATE compression with selectable stored fallback.
- [ ] Optional delta generation with explicit CPU, memory and depth budgets.

Acceptance: real Git clones/fetches loose-only, packed-only and mixed dumb
repositories. Strict Git import/index checks and exact-object comparisons cover
stored, cached, prepacked and any new encoder paths. All producers must opt into
cooperative leases; live lock files retain identity until namespace quiescence.

## 4. Server adapters and hosting

### Native

- [x] Explicit IPv4/IPv6 listeners, numeric CLI addresses, ephemeral ports,
  close-on-exec and IPv6-only sockets; real-Git clone/fetch on `::1`.
- [x] Fragmentation-safe bounded request storage, linear delimiter scanning,
  one-pass header metadata validation and gzip expansion into caller storage.
- [x] Blocking HTTP/1.0 close-delimited pull responses and accepted-socket
  send/receive timeouts, defaulting to ten seconds.
- [x] Inclusive complete-wire and decoded-body admission quotas independent of
  physical buffer capacity, with explicit error precedence and gzip integrity
  checks. Unset quotas preserve the original bounded-storage API.
- [x] Independent configurable accepted-socket read/write timeouts, including
  disabled settings, unchanged defaults and accepted-descriptor failure cleanup.
- [ ] Header-specific admission policy and CLI configuration, plus explicit
  request/idle deadlines where needed. Per-I/O timeouts are not total deadlines.
- [ ] HTTP/1.1 framing, chunked intake, keep-alive and pipelined/leftover-byte
  handling with strict framing ambiguity rejection and cancellation tests.
- [ ] Poll-based connections with independent caller-owned request/output state,
  bounded connection admission and slow/disconnected-client isolation.
- [ ] TLS integration through a documented host boundary and transport tests.

### Worker/Wasm

- [x] Fixed-memory start/read/cancel ABI, streamed response callbacks, pinned
  immutable snapshots and independent instances for overlapping responses.
- [x] Versioned canonical repository import in both formats: 64 objects, eight
  refs and 128 KiB of complete resident bodies, alongside compatible legacy frames.
- [x] Optional inline/KV/R2 immutable metadata snapshots, awaited publication,
  bounded authenticated reads, expiry and isolate-safe retries without recapture.
- [x] Configurable snapshot retention, documented KV propagation/R2 cleanup,
  explicit public header/binding selection and Access-credential exclusion.
- [x] Optional tree-shaped metadata, encoded-key files, versioned records and
  preservation of existing flat snapshot IDs. Deployment authentication is host-owned.
- [ ] An owned preparation/storage boundary for asynchronous selected-pack cache
  I/O, keeping request identity and buffers stable across awaits. Current Rust
  preparation/provider reads are synchronous; response pulls alone do not solve this.
- [ ] A bounded generated/host-backed provider beyond complete resident raw import.
- [ ] Diagnostics and measured preparation/pull CPU, JavaScript/backend/isolate
  memory and retention behavior under overlapping requests on a deployed Worker.

Acceptance: native and Worker hosts preserve snapshot identity across discovery,
upload-pack, retries and storage changes. Exercise fragmented I/O, slow clients,
concurrency, cancellation, timeouts, expiry, corrupt records and backend failures.
Local KV/R2-shaped tests do not establish deployment or propagation measurements.

## 5. Server parity tests and resource budgets

- [x] Pinned upstream HTTP feature map and auditable server acceptance matrix.
- [x] Reusable Git-created fixture export; six unchanged Git v2.53.0 `t5551`
  cases (2–7) run with native CGI substitution and a separate reference server,
  v0/v2 × SHA-1/SHA-256. External GPL tests/config stay unchanged.
- [x] Native rich fixture: 40-level trees, 32 changing historical blobs, merge,
  384 KiB binary, modes and nested tags, v0/v1/v2/dumb in both formats. The
  independent packed Git source contains deltas; git-zero exports raw objects.
- [x] Scaled Worker history fixture: 44 objects, seven refs, side/merge history,
  nine tree levels and a 96 KiB binary; both-format smart full/incremental clones.
- [x] Native A → B → A snapshot fetches with real Git, exact object/checkout
  checks, complete-pack hits and eviction; lower-level selected-cache race tests.
- [x] Seven bounded libFuzzer targets plus ordinary-Cargo seed replay and CI ASan
  smoke campaigns for packets, responses, routing, graphs, inflation, loose
  objects and pack entries/deltas; shallow/filter cancellation seeds are included.
- [x] Allocation counters for providers, validation, selection, pack/index/cache
  operations and decoding. Worker ABI tests enforce a 24-page ceiling and no growth.
- [x] Paired HTTP/object-read benchmark harness, pinned caller-repository inputs,
  matched/default encoder profiles, raw observations and opt-in wall/CPU/RSS caps.
  Bounded Linux process-tree sampling and CI caps reject incomplete observations.
- [x] Preserve the [historical Linux v6.12 snapshot evidence](tests/benchmarks/linux-v6.12-f71/README.md):
  91,865 objects, warm loopback clone, separate download/index/checkout and decoder
  experiments at `f71e266`, with raw data, provenance and reproduction limits.
- [ ] Bring Worker coverage to the full native fixture/HEAD/ref/shallow matrix,
  including both formats and independently checked protocol behavior.
- [ ] Complete unassisted dumb-object closure checks. Git's loose walker can omit
  a referenced empty tree; assisted import/fsck is recorded as assisted evidence.
- [ ] Worker HTTP selected-cache transitions and concurrent cold-miss/eviction
  comparisons on both hosts. Native synchronous single-producer HTTP transition,
  failure and eviction/reopen cases are covered; backend lease tests are separate.
- [ ] Broader upstream fixture substitution: more unchanged cases, Worker hosting
  and static dumb Alias routes. Retain licensing/provenance and no-fallback traces.
- [ ] Sustained native HTTP-framing and concurrent/cancelled host-state fuzzing;
  selected-cache/key/pack-index boundaries and regression seeds as APIs expand.
- [ ] Define and enforce total request-preparation work, including parsing,
  metadata probes and traversal, separately from graph bytes and buffer sizes.
  Test exhaustion before output and clean state reuse after failure/cancellation.
- [ ] Controlled cold-cache, WAN/concurrent and current-revision comparisons;
  investigate mapped/indexed native object storage and authenticated decoder costs.
- [ ] Full Linux history/incremental-fetch preparation and a committed reproducible
  driver for the separate kernel download/index/checkout/decoder experiments.
  The historical parentless snapshot and buffered frontend do not complete this.

Acceptance: each server feature has positive and negative independent-oracle
cases, enforced allocation/memory/work budgets and retained failure artifacts.
Keep correctness gates separate from optional timing experiments. Record client
and server CPU/wall, wire bytes, startup and buffer budgets; sampled RSS can miss
peaks and double-count shared mappings. Whole-isolate accounting remains open.

## 6. Server parity completion gate

- [ ] All in-scope server cases pass independent real-Git comparisons on native
  and Worker/Wasm hosts across the supported Git/platform matrix.
- [ ] Smart v0/v1/v2 and loose/packed dumb HTTP clone/fetch, HEAD/refs/tags,
  incremental/shallow/partial fetch and both object formats pass acceptance.
- [ ] Streaming, concurrent sessions, cancellation, storage/cache failures and
  exhausted limits preserve immutable repository identity and bounded resources.
- [ ] Capability/limit/host contracts match the implementation; unsupported and
  optional extensions remain explicit, with no untested advertised capability.

Compression and delta generation can follow the valid stored-pack baseline.
Start further client and write-server features only after this gate passes.

## 7. Read/write server support — deferred until server parity

The separate [Rust hosting experiment](examples/repository-host/README.md) tests
compressed chunk storage, atomic manifest publication and bounded inline compaction
with native Git as its codec. Its [local Cloudflare adapter](examples/repository-host/cloudflare/README.md)
accepts real pushes through workerd and reports object statistics, service operations
and cost scenarios. See the [design and limits](docs/hosting-design.md); this experiment does not
complete the portable server acceptance gate.

- [ ] Host-owned writable object storage and ref transactions with durability,
  locking and old/new-ID compare-and-swap semantics.
- [ ] `receive-pack` discovery, bounded commands/capabilities, pack ingestion and
  accurate status/error responses.
- [ ] Quarantine objects until checksums, canonical IDs, delta bases, graph
  connectivity and requested ref updates pass bounded validation.
- [ ] Authorized branch/tag creation/update/deletion, explicit fast-forward/force
  policy, and atomic advertisement only when the full ref transaction is atomic.
- [ ] Real-Git push comparisons: initial/incremental pushes, tags, deletions,
  stale IDs, multiple refs, force policy and atomic failures.
- [ ] Corrupt/truncated/disconnected input, denied updates, storage failure,
  concurrent writers, interruption and exact resource limits.

Acceptance: push then clone/fetch passes strict Git verification. Failed pushes
preserve promised object/ref visibility; retries retain old-ID checks. Each host
documents authentication, durability and transaction guarantees before enabling it.

## 8. Client work — deferred until server parity

- [x] V0 smart/dumb discovery, empty/unborn handling, HEAD/symbolic-ref resolution,
  exact branch/tag selection and SHA-1/SHA-256 IDs and pack trailers.
- [x] Multiple-tip and shallow request encoding; checksum-validated raw smart
  packs and validated dumb loose-object download with caller-owned buffers.
- [x] Incremental stored/fixed/dynamic zlib decoding, checksums, expansion limits,
  canonical loose envelopes and expected-ID validation.
- [x] Pack entry/CRC/ID verification and bounded offset/reference deltas;
  complete non-thin SHA-1 materialization into a caller arena and sorted ID index.
- [x] Borrowed commit/tree/tag views and shared checkout traversal, modes and
  blob/symlink validation; used by the kernel example.
- [ ] Persistent object sinks, external pack-index parsing/lookup and reusable
  indexed storage for complete/incremental smart fetches.
- [ ] SHA-256 complete-pack materialization with format-aware index storage,
  graph checks and allocation/work limits, preserving the existing SHA-1 API.
- [ ] Dumb graph traversal and packed/mixed dumb repositories, with bounded
  visited-object state and pack/index downloads.
- [ ] Complete native clone/materialization/checkout writer using host object
  sinks and explicit filesystem/name/symlink policy.
- [ ] Expanded capability/ACK/have negotiation, v1/v2, sideband/ERR decoding,
  filter requests and bounded incremental transport state.
- [ ] Push discovery, old/new-ID commands, outgoing packs, capability negotiation
  and per-ref status, including authorized force/deletion/atomic operations.
- [ ] Fetch/push interoperability against Git and git-zero with corrupt transfers,
  denied refs, interrupted I/O and exact resource limits.
- [ ] Native DNS/IPv6/TLS, HTTP/1.1/chunking, redirects, media-type checks and
  configurable transport limits. Current native client is blocking IPv4 HTTP/1.0.
- [ ] Streaming Wasm client ABI and transport callbacks without Rust allocation.
- [ ] GitHub Pages demo fetching this repository through Git HTTP, resolving its
  default tip, checking it out in memory and listing files; plan CORS/proxy access
  rather than replacing Git fetches with a GitHub API listing.
- [ ] Optional `git://` transport after HTTP compatibility.

Acceptance: complete HTTP clone/checkout, including packed dumb and delta packs;
push results independently verified with real Git. Existing primitives and tests
do not establish a complete clone or push implementation.

## 9. Experimental Linux kernel host — extensions deferred until server parity

- [x] Read-only pinned checkout and refs/message/tree views through shared client,
  pack materialization and checkout APIs, with bounded SHA-1 object caches.
- [x] Kernel HTTP/HTTPS sockets, DNS, bounded chunked framing, shallow smart
  imports/deltas, offline warm reads and QEMU regression fixtures.
- [ ] Authenticate HTTPS peers: the current example encrypts with handshake-only
  TLS and does not validate certificates, hostnames or server signatures.
- [ ] SHA-256 through the kernel ABI, IPv6 and private-repository authentication;
  these are not implied by the shared core's format/transport support.
- [ ] Persistent caching and scalable miss concurrency beyond the current
  per-mount serialized fetch path, with VFS/cache memory and cancellation budgets.
- [ ] Broader kernel/architecture build and runtime coverage beyond the documented
  x86-64 target; retain failed-mount/unmount cleanup and pinned/offline semantics.

See [the kernel example](examples/linux-kernel/README.md) for build requirements,
current transport limits, cache ownership and the unprivileged QEMU tests.
