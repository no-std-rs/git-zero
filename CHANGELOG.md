# Changelog

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
