# git-zero

Read-only Git HTTP **server and client** building blocks for synthetic repositories.
The Rust core is `#![no_std]`, uses no `alloc`, and requires no allocator. Callers
provide object data, scratch buffers, and I/O. Native networking uses `rustix`;
the Cloudflare demo runs the same core as an allocator-free Wasm module.

This is an initial working skeleton. It serves real Git clones and fetches, with
a deliberately small protocol surface. See [ROADMAP.md](ROADMAP.md) for the
remaining client, protocol, storage, and performance work.

## What works

- Borrowed in-memory files become blobs, a root tree, and one deterministic
  commit with **no parents**. `HEAD` points to `refs/heads/main`.
- Read-only smart HTTP discovery and `git-upload-pack`, using protocol v0.
  Git clients requesting v2 can fall back to the v0 advertisement.
- Ordinary Git HTTP, usually called **dumb HTTP**: `HEAD`, `info/refs`, branch
  refs, and zlib loose objects. No filesystem repository is needed.
- Complete SHA-1 packs generated on demand from unpacked objects. The encoder
  streams valid zlib/DEFLATE stored blocks, so it uses bounded memory but does
  not yet compress data or create deltas. Identical blobs are deduplicated.
- A transport-independent client that discovers refs, resolves `HEAD`, downloads
  and checksum-validates smart packs, and fetches dumb loose objects.
- An optional `PackCache` interface with caller-owned storage and versioned keys.
- A loopback IPv4 server/client adapter and a Cloudflare Workers Wasm demo.
- Parallel interoperability tests mixing real Git and git-zero clients/servers.

The client returns wire objects: raw packs for smart HTTP and zlib objects for
dumb HTTP. It does not yet inflate/index packs, resolve deltas, traverse a dumb
repository, or create a checkout. Push, shallow/filter requests, protocol v2,
SHA-256 repositories, and the standalone `git://` transport remain future work.

## Workspace

| Location | Responsibility |
| --- | --- |
| `src/` | Safe, allocation-free objects, snapshots, pack/cache, HTTP client/server |
| `crates/git-zero-rustix/` | `no_std` socket adapters; small `std` process wrapper |
| `examples/cloudflare-worker/wasm/` | `no_std` raw Wasm ABI with fixed buffers |
| `examples/cloudflare-worker/src/` | JavaScript host for Cloudflare metadata and HTTP |
| `tests/` | Core checks, real Git pack validation, parallel HTTP matrix |

The core and native adapter libraries never enable `std` or `alloc`. The native
CLI uses `std` for startup arguments, stdout, and diagnostics. JavaScript and the
test harnesses use their host runtimes' memory management; the no-allocation
guarantee applies to Rust core/adapter operations, not the surrounding platform.

## Native demo

Use Rust 1.85 or newer, Git with HTTP support, and Python 3 for tests.

```sh
cargo run -p git-zero-rustix --bin git-zero -- serve 8080
```

In another terminal:

```sh
git clone http://127.0.0.1:8080/repo.git demo-smart
GIT_SMART_HTTP=0 git clone http://127.0.0.1:8080/repo.git demo-dumb
cargo run -p git-zero-rustix --bin git-zero -- ls-remote http://127.0.0.1:8080/repo.git
cargo run -q -p git-zero-rustix --bin git-zero -- fetch http://127.0.0.1:8080/repo.git > snapshot.pack
```

The server prints its URL after binding; port `0` selects an OS-assigned port.
It serves a fixed `README.md` snapshot and handles connections serially. The
native client accepts `http://IPv4:port/path` URLs. Its current transport uses
HTTP/1.0 with connection close, bounded buffers, and socket timeouts; DNS, TLS,
redirects, and chunked responses need a richer transport.

## Embed the core

```rust
use git_zero::{File, Snapshot, http::{self, Request}, io::SliceWriter};

let files = [File { name: "hello.txt", contents: b"Hello from memory!\n" }];
let mut tree = [0; 512];
let mut commit = [0; 512];
let repository = Snapshot::new(&files, &mut tree, &mut commit)?;
let mut storage = [0; 4096];
let mut output = SliceWriter::new(&mut storage);
let response = http::serve(&repository, Request {
    method: "GET",
    path: "info/refs", // Relative to the repository URL.
    query: Some("service=git-upload-pack"),
    body: &[],
}, &mut output)?;
// Host sends response.status, response.content_type, and output.as_bytes().
```

`Snapshot` takes root-level filenames in byte-sorted order. Nested trees can be
provided by a custom `Repository` implementation; the snapshot convenience API
does not build them yet. A repository implementation must expose a stable,
complete reachable object set and a commit `HEAD` for its URL.

Use `pack::write` to stream directly to any `io::Write` implementation. Use
`cache::write` with a `PackCache` and pack-sized scratch buffer to reuse generated
packs. Cache hits do not need scratch space. Cache failures propagate. Async
platform caches such as Workers KV/R2 can wrap the host boundary; none is required
or configured in the demo yet.

Buffers are explicit limits. On capacity or protocol errors, discard partial
output before sending a response. Hosts own authentication, HTTP framing,
request limits, and concurrency. Send `Cache-Control: no-store` for discovery and
upload-pack, as both supplied adapters do.

## Cloudflare Workers demo

See [the demo README](examples/cloudflare-worker/README.md) for setup and its
snapshot URL format. It exports all available `request.cf` properties, `cf-*`
request headers, selected public environment variables, and a runtime descriptor
as four JSON files in one root commit. Internal platform state unavailable to
Workers cannot be exported. Environment bindings use an explicit allowlist
because the runtime does not distinguish string variables from secrets.

```sh
rustup target add wasm32-unknown-unknown
cd examples/cloudflare-worker
npm ci
npm run dev
```

Open the local Worker URL to obtain `clone_url`, then pass that URL to `git clone`.
The URL contains the captured public metadata, so every request and isolate can
reconstruct the same snapshot without a persistence service.

## Validation

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check -p git-zero --target wasm32-unknown-unknown --locked
cd examples/cloudflare-worker
npm test
```

`cargo test` runs the eight-way native interoperability matrix in parallel. All
HTTP tests need permission to bind/connect to loopback. The Worker tests clone
through the actual Wasm module with real Git over both HTTP protocols. See
[tests/README.md](tests/README.md) for selective runs, CI sharding, and the optional
Git upstream test runner. CI also checks the core against a bare-metal target,
which has neither `std` nor an allocator.

Protocol references: [Git HTTP](https://git-scm.com/docs/http-protocol),
[Git pack format](https://git-scm.com/docs/pack-format),
[Cloudflare Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/).

Licensed under MIT OR Apache-2.0.
