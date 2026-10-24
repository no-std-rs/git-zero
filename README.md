# git-zero

[![CI](https://github.com/no-std-rs/git-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/no-std-rs/git-zero/actions/workflows/ci.yml)
[![Runnerless CI: slowest job](https://badges.runnerlesshq.com/github.com/no-std-rs/git-zero/badge.svg)](https://app.runnerlesshq.com)
[![Git versus git-zero: discovery wall time](https://app.runnerlesshq.com/api/public/benchmark-ci/no-std-rs/git-zero/badge.svg)](https://app.runnerlesshq.com/api/public/benchmark-ci/no-std-rs/git-zero/main)

An allocation-free Git toolkit in safe `no_std` Rust: objects, packs, clients,
and servers. Callers supply repository data, buffers, storage, and I/O. The same
core runs on native systems, bare-metal targets, and WebAssembly.

The library crates use neither `std` nor `alloc`. Native CLI startup and the
JavaScript hosting layers have their own memory and runtime requirements.
This is a set of Git building blocks, with a demonstration CLI and host examples.

## Benchmarks

[![Git versus git-zero benchmark report](https://app.runnerlesshq.com/api/public/benchmark-ci/no-std-rs/git-zero/main.svg)](https://app.runnerlesshq.com/api/public/benchmark-ci/no-std-rs/git-zero/main)

Every main push and PR measures discovery, clone and fetch against pinned Git
with matched compression and delta settings. Runnerless processes the original
observations and publishes the report; PR runs also receive a benchmark comment.
The chart shows warm-cache descriptive medians on GitHub-hosted Linux runners.
See [collection, retention and manual reruns](docs/benchmarks.md) for the scope.

## Choose a crate

| Crate | Use it for | API |
| --- | --- | --- |
| [git-zero](https://crates.io/crates/git-zero) | Combined API with optional client and server reexports | [git_zero](https://docs.rs/git-zero) |
| [git-zero-core](https://crates.io/crates/git-zero-core) | Objects, repositories, packs, decoding, and caller-owned caches | [git_zero_core](https://docs.rs/git-zero-core) |
| [git-zero-client](https://crates.io/crates/git-zero-client) | Fetching through a host-provided HTTP transport | [git_zero_client](https://docs.rs/git-zero-client) |
| [git-zero-server](https://crates.io/crates/git-zero-server) | Smart/dumb HTTP serving and opt-in receive-pack primitives | [git_zero_server](https://docs.rs/git-zero-server) |
| [git-zero-rustix](https://crates.io/crates/git-zero-rustix) | Native file/socket adapters and the `git-zero` CLI | [git_zero_rustix](https://docs.rs/git-zero-rustix) |

All five crates share version `0.1.0` and require Rust 1.85 or newer.

```toml
[dependencies]
git-zero = "0.1.0"
```

The facade enables `read`, `write`, `client`, and `server` by default. For a
fetch-only dependency, select just the client:

```toml
git-zero = { version = "0.1.0", default-features = false, features = ["client"] }
```

For example, construct a repository from borrowed files and fixed buffers:

```rust
use git_zero::{File, Repository, Snapshot};

fn main() -> git_zero::Result<()> {
    let files = [File { name: "hello.txt", contents: b"Hello from git-zero!\n" }];
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&files, &mut tree, &mut commit)?;
    assert_eq!(repository.object_count(), 3); // Commit, tree, and blob.
    Ok(())
}
```

The [embedding guide](https://github.com/no-std-rs/git-zero/blob/main/docs/embedding.md)
shows custom repositories, streaming, validation, and host-owned storage.

## What works

- SHA-1/SHA-256 objects, refs, trees, commits, and synthetic snapshots.
- Smart HTTP serving over protocol v0/v1/v2, plus loose and packed dumb HTTP.
- Bounded fetch selection, shallow/history filters, and streamed responses.
- Transport-independent fetching, zlib/pack decoding, and delta resolution.
- Caller-owned selected-pack caches, a native disk backend, and opt-in Worker KV/R2 storage.
- Native file/socket adapters, request/response deadlines, and bounded chunked intake.
- Durable Cloudflare hosting with authenticated push, clone and fetch, atomic
  ref updates, compressed packs, thin pushes and cold-start recovery.

Snapshot HTTP APIs serve caller-provided read-only repositories. The separate
[repository host](https://github.com/no-std-rs/git-zero/blob/main/examples/repository-host/README.md) accepts pushes through our
own `no_std` Rust Wasm codec and stores data in Cloudflare R2 and SQLite Durable
Objects. Its local tests use actual Wrangler bindings and ordinary Git clients.
Full Linux-scale hosting and deployed resource costs remain unverified; the
host guide records the platform limits and remaining compatibility gaps.

## Try the native CLI

Install the native adapter package, which supplies the `git-zero` executable:

```sh
cargo install git-zero-rustix --version 0.1.0 --locked
git-zero serve 8080
```

For a source checkout, run from the repository root instead:

```sh
cargo run -p git-zero-rustix --bin git-zero -- serve 8080
```

Then, in another terminal:

```sh
git clone http://127.0.0.1:8080/repo.git demo
```

This serves a fixed in-memory snapshot. The `git-zero` executable also works as
the Git subcommand `git zero`. The native demo uses HTTP with numeric addresses;
the client library lets the host supply a different transport.
See [usage](https://github.com/no-std-rs/git-zero/blob/main/docs/usage.md) for client
commands, file-backed storage, crate features, and the Worker demo.

## Documentation

- [Documentation index](https://github.com/no-std-rs/git-zero/blob/main/docs/README.md)
- [Current architecture](https://github.com/no-std-rs/git-zero/blob/main/docs/design.md)
- [Source map for upstream Git developers](https://github.com/no-std-rs/git-zero/blob/main/docs/upstream-mapping.md)
- [Embedding and repository APIs](https://github.com/no-std-rs/git-zero/blob/main/docs/embedding.md)
- [Object formats and decoding](https://github.com/no-std-rs/git-zero/blob/main/docs/object-decoding.md)
- [Development and validation](https://github.com/no-std-rs/git-zero/blob/main/docs/validation.md)
- [Release and package publishing](https://github.com/no-std-rs/git-zero/blob/main/docs/releases.md)

For plans and project workflow, see the
[roadmap](https://github.com/no-std-rs/git-zero/blob/main/ROADMAP.md) and
[contribution guide](https://github.com/no-std-rs/git-zero/blob/main/CONTRIBUTING.md).

Licensed under MIT OR Apache-2.0.
