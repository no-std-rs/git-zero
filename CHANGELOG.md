# Changelog

## Unreleased

- Repository browse results carry incarnation-bound, HKDF-derived object proofs
  for immediate raw-ID navigation. Additive publications retain proof/index
  progress; rewrites/deletions rebuild outward from current tips. Commit graph
  links come from typed stored edges, including for late lookalike headers.
- Gateway fetch wants default to current reachability; whole-catalog access is
  explicit. Rewritten bytes still require repository deletion for reclamation.
- Fetches, browse and private push intake/validation run concurrently with
  per-job SQL scratch and ref snapshots. Only publication CAS is exclusive;
  deletion fences/cancels jobs and waits for their storage I/O. Identical gateway
  full clones coalesce into one bounded generation-keyed R2 pack build.
- Browse runs alongside fetch streams/private intake. Gateway streams use a
  throughput floor instead of a default five-minute cutoff; request bodies have
  a control-prefix deadline and minimum-throughput pack bound after admission.
  A 64 MiB namespace-fair pool reuses charged codec arenas, sizes reservations
  by route, protects metadata capacity and never preempts active jobs. Cached
  readers use a separate concurrency ceiling. Shared builds outlive their first
  client and use progress bounds; failed pre-header followers can fall back.
- Small fetch fallbacks use bounded memory pins; gateway read scratch defaults
  to 32 MiB per request/64 MiB per repository, with pre-header progress bounds and retryable 503 on scratch contention.
  Refusal tails drain after releasing repository jobs, at most two per object;
  unauthenticated tails are limited to 1 MiB/5 seconds.

- Policy refusals drain bounded upload tails before Git status; early hidden,
  read-only and busy POST replies also discard bounded transport bytes. Authorized defaults
  allow 64 MiB/30 seconds; larger or stalled rejected uploads may still disconnect.

- Fetch fallback reconstruction is prepared before response headers. Read
  scratch is reclaimed separately and never blocked by stored-size caps or
  pending legacy accounting; push-validation scratch retains write quotas.

- Repository host: remove cross-repository admission blocking. Gateway jobs use
  smaller default caches and bounded per-repository queues/response throughput.
  Token deployments retain their previous unlimited pending queue, caches and idle
  limit, exempt from byte admission by default (explicit pool configuration opts in);
  the experimental isolate-wide four-job/64-request limits are removed.
- Browse reads rooted at refs no longer rebuild repository-wide reachability.
  Raw IDs reuse an incremental generation index, returning a retryable building
  response while proof is unavailable; signed cursors preserve continuation safety.

- Repository host: validate storage prefixes during alarm recovery, support
  configured EU namespace identity, parse environment booleans without truthiness,
  avoid durable state for missing repositories and reclaim earlier schema-only
  namespaces. Codec provenance references an immutable main ancestor and checks
  its build-input fingerprint rather than relying on a PR-only commit.

## [0.2.0](https://github.com/no-std-rs/git-zero/compare/v0.1.0...v0.2.0) (2026-10-25)


### Features

* **ci:** publish Git comparisons through Runnerless on main and PRs ([#64](https://github.com/no-std-rs/git-zero/issues/64)) ([4daf207](https://github.com/no-std-rs/git-zero/commit/4daf207b41b1761bcdd1209e6c9e5a5bf5c5cd23))

Release notes are maintained by Runnerless release-please.

## 0.1.0

First public release of `git-zero`, `git-zero-core`, `git-zero-client`,
`git-zero-server`, and `git-zero-rustix`.

- Safe `no_std` libraries without `alloc`, using caller-owned buffers, storage,
  repository views, and I/O.
- SHA-1/SHA-256 Git objects and refs, pack/index generation, bounded zlib and
  delta decoding, repository validation, and checkout traversal primitives.
- Transport-independent v0 HTTP fetching, smart HTTP serving over v0/v1/v2,
  loose/packed dumb publication, bounded shallow/history filters, and streaming.
- Verified selected-pack caches, native disk storage, opt-in Worker KV/R2
  caching, and request-local cancellation.
- Native file/socket adapters and demonstration CLI, including request/response
  deadlines and opt-in bounded chunked request intake.
- Opt-in receive-pack primitives and a separate Cloudflare repository host
  demonstrating authenticated push/clone/fetch with R2 and atomic SQLite refs.

The CLI is a demonstration, and the host examples retain the provider, transport,
and deployment limits documented in their guides and `ROADMAP.md`.
