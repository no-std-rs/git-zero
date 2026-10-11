# Roadmap

The goal is a read-only Git client/server library with an allocation-free,
`no_std` core, stable synthetic repositories, on-demand packing, optional caching,
and host adapters for native `rustix`, Wasm, and Cloudflare Workers.

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
- [x] Native `rustix` libraries and local CLI examples for both roles.
- [x] Allocator-free Wasm build and stateless Cloudflare metadata snapshot demo.
- [x] Parallel real-Git interoperability matrix and Wasm clone tests.
- [x] README, testing instructions, and CI checks for native/Wasm/bare-metal builds.

## 1. Object and repository storage

- [x] Nested trees, executable files, symlinks, empty trees, and configurable refs.
- [x] Caller-owned object metadata/index arrays to avoid repeated scans and hashing.
- [x] Bounded readers for large or generated objects; explicitly size scratch and
  resumable state, and avoid requiring a whole object in one borrowed slice.
- [x] Incremental root-entry snapshot builder with configurable commit identity/message/time.
- [x] Validation of custom repository object graphs, tree order, and reachability.
- [x] Native file-backed object provider using `rustix`, with optional prepacked data.

Acceptance: Git `fsck --strict` accepts nested and generated snapshots; deterministic
inputs yield identical IDs on native and Wasm without enabling an allocator.

## 2. Client object consumption

- [ ] Incremental zlib inflation into caller buffers with output/ratio limits.
- [ ] Validate loose-object headers, lengths, and IDs before exposing contents.
- [ ] Pack entry reader, CRC checks, object IDs, and bounded delta resolution.
- [ ] Caller-owned pack indexes and object sinks for complete smart fetches.
- [ ] Dumb object-graph traversal, `objects/info/packs` and pack/index downloads,
  with an explicit memory bound for visited objects and traversal state.
- [ ] Correct empty/unborn remotes, symbolic refs, branch/tag selection, and SHA-256.
- [ ] A complete clone/materialization example using a host object sink and checkout.

Acceptance: git-zero imports and checks out a real Git repository through both
HTTP protocols, including packed dumb repositories and delta-compressed packs.

## 3. Protocol compatibility

- [ ] Broader v0 capability negotiation, ACK/have negotiation, sideband and error packets.
- [ ] Protocol v1/v2 (`ls-refs`, `fetch`) with accurate capability advertisements.
- [ ] Incremental requests and responses, backpressure, cancellation, and resume points.
- [ ] Shallow/filter requests with bounded traversal and documented rejection paths.
- [ ] Proper method/content-type checks and a wider malformed-request corpus.
- [ ] Optional `git://` transport after HTTP compatibility is established.

Acceptance: supported cases from upstream HTTP tests have mapped, reproducible
git-zero coverage; unsupported operations return explicit errors.

## 4. Packing and cache backends

- [ ] Bounded actual DEFLATE compression, with stored blocks as a selectable fallback.
- [ ] Reuse prepacked entries and existing packs when their closure matches the request.
- [ ] Optional delta generation with strict CPU/memory budgets.
- [ ] Stream pack index creation alongside packs for dumb HTTP reuse.
- [ ] Versioned keys incorporating object format, encoder choices, and requested closure.
- [ ] Fixed-memory example cache, native disk cache, and async KV/R2 host adapters.
- [ ] Cache integrity validation, eviction, cold-miss coordination, and observability.

Acceptance: cache miss/hit/reuse paths produce equivalent objects; cache failures,
corrupt entries, and concurrent writers have tested behavior.

## 5. Platform adapters and demo

- [ ] Native DNS/IPv6/TLS, HTTP/1.1 framing, chunked transfer, redirects, and configurable limits.
- [ ] Poll-based `rustix` server with independent per-connection caller-owned state.
- [ ] Streaming Wasm client/server ABI and transport callbacks without Rust allocation.
- [ ] GitHub Pages demo at `no-std-rs/git-zero`: run the Wasm client in the browser,
  fetch this project's own repository from GitHub, resolve the default branch's
  tip ref, check out that commit in memory, and list every file in its tree.
  Plan browser CORS support or an optional HTTP proxy so the demo exercises the
  Git client rather than substituting a GitHub API file listing.
- [ ] Worker integration with async storage, streaming responses, and isolate-safe reuse.
- [ ] Replace long embedded snapshot URLs with optional immutable persisted snapshots;
  preserve stable IDs across discovery and downloads, retries, and isolate changes.
- [ ] Tree-shaped Cloudflare metadata and public binding projection, with deployment
  authentication and explicit public variable selection.
- [ ] Configurable retention, diagnostics, and CPU/memory measurements for Workers.

Acceptance: native and Worker hosts share protocol behavior; overlapping requests
cannot overwrite each other's buffers or change repository identities.

## 6. Test expansion

- [x] Map upstream `t5550`/`t5551` cases to supported/planned/out-of-scope features;
  see `tests/upstream-http.md` for the pinned reference and independent comparisons.
- [ ] Reusable upstream fixture adapter where Git's harness allows replacing a server;
  record GPL provenance for any copied tests rather than copying them silently.
- [ ] Empty/bare repositories, arbitrary refs, tags, large binary blobs, deep histories,
  nested trees, aggressive packing, and deltas in the interoperability matrix.
- [ ] Incremental fetch after changing a backend snapshot, pack reuse and eviction races.
- [ ] Fuzz packet lines, HTTP routing, zlib/pack parsers, and object-graph validation.
- [x] Allocation tracking for native file providers, repository validation, prepacked
  verification, and pack generation/serving.
- [ ] Fixed-budget benchmarks and Wasm memory growth checks.
  `tests/benchmark.py` uses a configurable synthetic Git repository and identical
  file-provider export through a shared Rust HTTP host. Paired discovery/clone/fetch
  measurements retain client and server wall/CPU/RSS, child usage, response bytes,
  startup costs and buffer budgets. Encoded files share a sendfile path with bounded
  fallbacks; optional Rust loose caching charges preparation to startup. Wasm and
  simultaneous process-tree memory remain explicit future work.
- [ ] Opt-in local Linux kernel repository benchmark using a caller-provided clone
  and pinned revision; compare supported clone/fetch and object-reading workloads
  with real Git over loopback, recording wall/CPU time, peak RSS, throughput,
  and caller-owned buffer budgets for cold/warm runs.
- [x] Protocol traces and retained artifacts on failure; CI matrix over Git versions
  and Linux/macOS, with jobs isolated by temporary paths and ephemeral ports.

Production write support (`receive-pack`) is outside the initial read-only scope.
