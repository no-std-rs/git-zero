# git-zero

[![Runnerless CI: slowest job](https://badges.runnerlesshq.com/github.com/no-std-rs/git-zero/badge.svg)](https://app.runnerlesshq.com)

[![CI](https://github.com/no-std-rs/git-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/no-std-rs/git-zero/actions/workflows/ci.yml)

Read-only Git HTTP **server and client** building blocks for synthetic repositories.
The Rust core is `#![no_std]`, uses no `alloc`, and requires no allocator. Callers
provide object data, scratch buffers, and I/O. Native networking uses `rustix`;
the Cloudflare demo runs the same core as an allocator-free Wasm module.

This is an initial working skeleton. It serves real Git clones and fetches, with
a deliberately small protocol surface. See [ROADMAP.md](ROADMAP.md) for the
remaining client, protocol, storage, and performance work.

Development uses protected pull requests, parallel Git interoperability tests,
and a basic Runnerless PR policy program. See [CONTRIBUTING.md](CONTRIBUTING.md)
for checks and repository connection requirements.

## What works

- Borrowed entries become nested trees, regular/executable files, symlinks,
  and one deterministic commit with **no parents**. Empty trees are supported.
  `HEAD` defaults to `refs/heads/main`; callers can configure branch/tag aliases.
- Incremental root-entry construction in caller-owned slots, with configurable
  author/committer identities, Unix times, timezones, and exact commit messages.
- Read-only smart HTTP discovery and `git-upload-pack`, using protocol v0.
  Git clients requesting v2 can fall back to the v0 advertisement.
- Ordinary Git HTTP, usually called **dumb HTTP**: `HEAD`, `info/refs`, branch
  refs, and zlib loose objects. No filesystem repository is needed.
- Complete SHA-1 packs generated on demand from unpacked objects. The encoder
  streams valid zlib/DEFLATE stored blocks, so it uses bounded memory but does
  not yet compress data or create deltas. Identical blobs and subtrees are deduplicated.
- Optional caller-owned object metadata and ID indexes for constant-time
  enumeration and binary-search lookup without repeated scans or hashing.
- Bounded readers and resumable hashing for large/generated objects, with
  caller-sized scratch for pack and loose-object responses.
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

`Snapshot::new` takes root-level regular files in byte-sorted order.
`Snapshot::from_entries` accepts a hierarchy of borrowed `Entry` slices:

```rust
use git_zero::{Entry, EntryKind, Snapshot};

let scripts = [Entry { name: "hello", kind: EntryKind::Executable(b"#!/bin/sh\necho hello\n") }];
let entries = [
    Entry { name: "bin", kind: EntryKind::Tree(&scripts) },
    Entry { name: "empty", kind: EntryKind::Tree(&[]) },
    Entry { name: "hello", kind: EntryKind::Symlink(b"bin/hello") },
];
let mut trees = [0; 1024];
let mut commit = [0; 512];
let refs = ["refs/heads/release", "refs/tags/v1"];
let repository = Snapshot::from_entries(&entries, &mut trees, &mut commit)?
    .with_refs("refs/heads/release", &refs)?;
```

Sort each tree's entries using `Entry::cmp_name`: directories compare as if their
names end in `/`, so `foo.bar` precedes the directory `foo`. Duplicate names and
invalid path components are rejected at every level. The caller's tree buffer
holds all raw tree bodies plus four bytes per tree, including empty/duplicate
trees. Tree depth is limited to 64 levels including the root; excess depth returns
`Unsupported`. All refs alias the snapshot's single commit; ref names must be
valid, unique, byte-sorted, include the default branch, and have no path conflicts.

`SnapshotBuilder` accepts root entries incrementally in any arrival order and
inserts them in Git order. Initialize caller-owned slots with `Entry::EMPTY`;
only occupied slots enter the snapshot. Borrowed child trees are preassembled
and validated at finish. It does not create directories from path strings.

```rust
use git_zero::{CommitIdentity, CommitOptions, Entry, EntryKind, SnapshotBuilder};

let mut entries = [Entry::EMPTY; 8];
let mut trees = [0; 1024];
let mut commit = [0; 512];
let mut builder = SnapshotBuilder::new(&mut entries, &mut trees, &mut commit);
builder.push(Entry { name: "z.txt", kind: EntryKind::File(b"last") })?;
builder.push(Entry { name: "a.txt", kind: EntryKind::File(b"first") })?;
let identity = CommitIdentity {
    name: "Snapshot Author", email: "author@example.invalid",
    timestamp: 1_700_000_000, timezone: "+0000",
};
let repository = builder.finish_with_commit(CommitOptions {
    author: identity, committer: identity, message: "Generated snapshot\n",
})?;
```

Failed insertions preserve occupied slots: duplicates return `UnsortedFiles`,
invalid components return `InvalidFileName`, and full capacity returns
`BufferTooSmall`. Finishing consumes the builder; discard partial tree/commit
bytes on error. Slot storage and object contents remain borrowed by the snapshot,
but commit metadata is serialized and need only live until finish. Insertion is
linear in the occupied root entries and uses no extra scratch or allocator.

`finish()` retains the historical commit bytes and IDs. For preassembled inputs,
`Snapshot::new_with_commit` and `Snapshot::from_entries_with_commit` accept the
same options while retaining each constructor's tree-storage layout. Names and
emails must be nonempty and contain no control bytes or angle brackets. Messages
are preserved exactly, with no added newline, and may not contain NUL. Unix times
are unsigned seconds up to `i64::MAX`; timezone offsets have a sign, hours 00–23,
and minutes 00–59 and do not adjust those seconds. Malformed metadata returns
`InvalidObject`. Very large timestamps need not be displayable by every host Git.
Names must fit smart discovery packets: at most 65,474 bytes per ref and 65,457
bytes for the default branch (which also appears in the HEAD symref capability).
Changing refs leaves object IDs and pack-cache keys stable.
`http::Request::path` is a URL path: percent-encode reserved bytes in ref URLs.
The server decodes escapes once when matching ref names, preserving literal `+`.

Git checkout does not create directories for empty trees. Git's dumb HTTP walker
can also omit the empty-tree object from disk (observed with Git 2.53), causing
`fsck --strict` to report it missing. The loose-object endpoint serves it; clients
needing a complete disk object store must explicitly fetch that object or use
smart HTTP. See [tests/README.md](tests/README.md) for the interoperability check.

A custom `Repository` implementation must expose a stable, complete reachable
object set and a commit `HEAD` for its URL. Its refs are aliases of that commit.
Bodies can be borrowed slices or generated/read from storage by offset.

For repeated requests, wrap a snapshot or custom repository in `IndexedRepository`:

```rust
use git_zero::IndexedRepository;

// `repository` is the snapshot built above. Choose capacities for its objects.
let mut objects = [None; 16];
let mut by_id = [0; 16];
let repository = IndexedRepository::new(&repository, &mut objects, &mut by_id)?;
```

Each array needs at least `repository.object_count()` slots. Indexing reads the
source count once, reads and hashes each object once, and sorts the ID index in
place without allocating. It borrows object bodies and preserves object order,
refs, IDs, and pack bytes. Subsequent enumeration and `object_id` access use the
metadata directly; `find` uses binary search. `metadata` exposes the cached ID,
kind, and borrowed body. Indexing pays the source's scan cost once; it does not
accelerate snapshot construction or validate custom object graphs. Insufficient
capacity returns `BufferTooSmall`; missing source objects return `ObjectNotFound`.
The native demo builds its index once before accepting requests.

For bodies that cannot fit in a borrowed slice, implement `Repository::object_info`,
`read_object`, and `object_id`. `object_info` reports the kind and exact 64-bit body
size; `read_object` fills only the provided buffer at the requested 64-bit offset.
Short reads are supported. Reads must be repeatable and independent of previous
calls so multiple readers can interleave. `object_id` supplies stable IDs, and
`find_index` can optionally use a provider's own ID index. The borrowed `object`
and `find` methods may return `None` for streamed bodies; `IndexedRepository`
requires borrowed bodies.

```rust
use git_zero::{ObjectHasher, ObjectReader, pack};

let mut scratch = [0; 1024]; // Choose the read budget, independent of object size.
let mut reader = ObjectReader::new(&repository, 0)?;
let length = reader.read(&mut scratch)?;
// Consume scratch[..length]; retain reader to resume, or seek to another offset.
let mut hasher = ObjectHasher::new(&repository, 0)?;
let id = loop {
    if let Some(id) = hasher.step(&mut scratch)? { break id; }
    // Retain hasher here to yield between reads.
};
pack::write_with_scratch(&repository, &mut scratch, &mut output)?;
```

`ObjectReader` stores its cursor and metadata inline; `ObjectHasher` adds SHA-1
state. `core::mem::size_of` gives their exact state budgets for a target. Neither
owns scratch or allocates. A read/hash step performs at most one provider read;
EOF needs no buffer, and nonempty bodies need at least one scratch byte. Early EOF
and overreported read counts return `InvalidObject`; backend errors propagate.
Read errors leave the cursor/hash unchanged for retry. Providers must report exact
lengths and stable contents; this interface does not validate object graphs.
`pack::write_loose_with_scratch` and `http::serve_with_scratch` also accept an
explicit read budget. Pack/loose bytes are independent of scratch size and short
reads. The convenience `pack::write` and `http::serve` reserve 1 KiB of stack
scratch for streamed bodies; borrowed bodies still write directly.

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
