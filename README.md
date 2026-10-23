# git-zero

[![CI](https://github.com/no-std-rs/git-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/no-std-rs/git-zero/actions/workflows/ci.yml)
[![Runnerless CI: slowest job](https://badges.runnerlesshq.com/github.com/no-std-rs/git-zero/badge.svg)](https://app.runnerlesshq.com)

Git HTTP client and server building blocks in safe `no_std` Rust, without
`alloc`. Callers supply repository data, buffers, storage, and I/O. The same core
runs natively and as a Cloudflare Workers Wasm module.

## What works

- SHA-1/SHA-256 objects, refs, trees, commits, and synthetic snapshots.
- Smart HTTP serving over protocol v0/v1/v2, plus dumb HTTP loose objects.
- Bounded fetch selection, absolute/relative deepening, filters, and streamed responses.
- Transport-independent fetching, zlib/pack decoding, and delta resolution.
- Caller-owned selected-pack caches and a native disk backend.
- Native file/socket adapters and a Cloudflare metadata repository demo.
- Durable Cloudflare hosting with authenticated push, clone and fetch, atomic
  ref updates, compressed packs, thin pushes and cold-start recovery.

Snapshot HTTP APIs serve caller-provided read-only repositories. The separate
[repository host](examples/repository-host/README.md) accepts pushes through our
own `no_std` Rust Wasm codec and stores data in Cloudflare R2 and SQLite Durable
Objects. Its local tests use actual Wrangler bindings and ordinary Git clients.
Full Linux-scale hosting and deployed resource costs remain unverified; the
host guide records the platform limits and remaining compatibility gaps.

## Try it locally

Requires Rust 1.85+ and Git with HTTP support. From the repository root:

```sh
cargo run -p git-zero-rustix --bin git-zero -- serve 8080
```

Then, in another terminal:

```sh
git clone http://127.0.0.1:8080/repo.git demo
```

This serves a fixed in-memory snapshot. See [usage](docs/usage.md) for client
commands, file-backed storage, crate features, and the Worker demo.

## Documentation

- [Documentation index](docs/README.md)
- [Current architecture](docs/design.md)
- [Source map for upstream Git developers](docs/upstream-mapping.md)
- [Embedding and repository APIs](docs/embedding.md)
- [Object formats and decoding](docs/object-decoding.md)
- [Development and validation](docs/validation.md)

For plans and project workflow, see [ROADMAP.md](ROADMAP.md),
and [CONTRIBUTING.md](CONTRIBUTING.md).

Licensed under MIT OR Apache-2.0.
