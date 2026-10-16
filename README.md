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
- Read-only smart HTTP discovery and `git-upload-pack` over protocol v0/v1/v2,
  including stateless v2 `ls-refs` and `fetch` with bounded selected packs.
- Ordinary Git HTTP, usually called **dumb HTTP**: `HEAD`, `info/refs`, branch
  refs, and zlib loose objects. No filesystem repository is needed.
- SHA-1 and SHA-256 repositories with format-specific object IDs, refs and packs.
  Smart HTTP advertises the repository's object-format capability.
- Complete packs generated on demand from unpacked objects. The encoder
  streams valid zlib/DEFLATE stored blocks, so it uses bounded memory but does
  not yet compress data or create deltas. Identical blobs and subtrees are deduplicated.
- Optional caller-owned object metadata and ID indexes for constant-time
  enumeration and binary-search lookup without repeated scans or hashing.
- Bounded readers and resumable hashing for large/generated objects, with
  caller-sized scratch for pack and loose-object responses.
- Pull-based pack/HTTP response cursors with bounded inline state, backpressure,
  cancellation and caller-owned request intake. Native close-delimited responses
  and the Worker start/read/cancel ABI stream without response-sized buffers.
- Opt-in validation and indexing of custom repositories, checking object IDs,
  tree order and links, commit parents, tag targets, and exact reachability from
  HEAD and independent refs. Symbolic ref chains and final peeled tags are supported.
- Bounded multiple-want selection and packing of just the requested closures,
  using caller-owned traversal arrays, scratch and a graph-read byte limit.
- A transport-independent client that discovers refs, resolves `HEAD`, downloads
  and checksum-validates smart packs, fetches several tips with optional shallow
  negotiation, and fetches dumb loose objects.
- Incremental zlib inflation with caller-owned history/output buffers, stored/
  fixed/dynamic DEFLATE support, Adler-32 validation and output/expansion limits.
- Loose-object envelope validation before body exposure, plus a pack entry
  reader with wire CRCs, canonical object IDs and bounded offset/reference deltas.
- Complete pack materialization into caller-owned bodies and a sorted ID index,
  plus borrowed commit messages, tree entries and annotated-tag targets.
- An optional `PackCache` interface with caller-owned storage and versioned keys.
- IPv4/IPv6 server listeners, an IPv4 client adapter and a Cloudflare Workers Wasm demo.
- Native file-backed raw objects and verified prepacked responses, with positional
  reads and caller-owned scratch, using `rustix` without `std` or `alloc`.
- Parallel interoperability tests mixing real Git and git-zero clients/servers.

The client downloads wire objects: raw packs for smart HTTP and zlib objects for
dumb HTTP. Hosts can decode them and materialize complete packs with shared core
APIs; the kernel example uses these to expose cached snapshots and refs. A dumb
repository walker and a native checkout writer remain work. Push, filter
requests, protocol v2, and standalone `git://` transport
remain future work. Shallow fetch is supported by the client; this library's
server still serves complete selected packs.

## Object formats

`ObjectFormat::{Sha1, Sha256}` selects one fixed format for a repository.
`ObjectId::{Sha1([u8; 20]), Sha256([u8; 32])}` preserves that format and all ID
bytes. Replace the former tuple constructor with `ObjectId::Sha1(bytes)`, and
replace `.0` with `as_bytes()`. `from_hex` accepts exactly 40 or 64 hex digits;
`to_hex()` returns fixed storage exposing only those active bytes.

Existing snapshot constructors and `Object::id()` default to SHA-1 and retain
their object bytes and IDs. Use `Snapshot::new_with_format`,
`Snapshot::from_entries_with_format` or `SnapshotBuilder::finish_with_format`
for SHA-256; corresponding `*_with_commit_and_format` methods accept custom
metadata. Tree IDs occupy 32 binary bytes and commit/tag links use 64 hex digits.
Tree storage therefore needs another 12 bytes per entry compared with SHA-1.
Empty repositories use `EmptyRepository::new_with_format`.

Custom SHA-256 providers must override `Repository::object_format()` (its default
is SHA-1), return IDs in that format, and use its zero ID for unborn HEAD.
Indexes and validated/ref wrappers forward the provider's format. Validation
rejects mixed IDs, wrong-width links and hash mismatches. `FileRepository` takes
its format from the supplied HEAD ID, including an unborn zero ID.

Smart v0 discovery advertises `object-format=sha1` or `object-format=sha256`.
The first want may explicitly select that format; mismatches and unadvertised
capabilities fail before pack output. [Git's v0 client](https://github.com/git/git/blob/v2.53.0/fetch-pack.c)
selects the advertised
format without repeating it on wants, so omission also uses the repository's
format. Every want/have still requires the correct ID width. Dumb refs and loose
paths use 40 or 64 digits. Independent real-Git clone/fetch tests cover both.
Client discovery rejects mixed ID formats and capabilities that conflict with
the advertised IDs, including the empty-repository marker.
Packs use a 20-byte SHA-1 or 32-byte SHA-256 trailer; HEAD cache keys include the
ID's format. This does not translate objects between formats.

## Incremental zlib inflation

`zlib::Inflater` decodes downloaded zlib bytes without allocation or external
compression dependencies. It borrows one caller-owned `zlib::WINDOW_SIZE`
(32 KiB) history array; its resumable state is less than 2 KiB. Input and output
chunks can be as small as one byte, and overlapping back-references work across
history wraps and block boundaries.

Construct the decoder with `InflateLimits { max_output, max_ratio, ratio_slack }`.
Before each output byte it enforces both the absolute output cap and
`output <= consumed_input * max_ratio + ratio_slack`, using saturating arithmetic.
Only consumed bytes from the current zlib stream contribute to the ratio;
unconsumed trailing bytes cannot enlarge the budget. The explicit slack permits
compressible prefixes. Zero ratio permits at most the slack bytes. These checks
behave identically across chunk partitions.

`update(input, output, end_of_input)` returns consumed/written counts and
`NeedInput`, `NeedOutput` or `Finished`. Advance input by the consumed count,
retain the decoder and provide the unconsumed suffix on the next call. Set
`end_of_input` when the supplied slice contains the last available bytes;
missing input then reports `InvalidZlib`, while full output remains resumable.
Empty chunks are accepted, and exact output budgets allow parsing the final
block and checksum. `Finished` leaves subsequent pack entries or trailing bytes
untouched; callers requiring one complete loose-object response must reject any
remaining suffix.

Stage output until `Finished`, which validates the zlib header, block structure,
back-reference distances and Adler-32. Discard staged data on error; a failing
call can modify its output buffer. Malformed/truncated data returns
`InvalidZlib`, exhausted expansion budgets return `LimitExceeded`, and preset
dictionaries return `Unsupported`. Errors are terminal until `reset(limits)`.
The standalone inflater leaves Git envelope validation to the object decoder.
See the [API example](crates/git-zero-core/src/zlib.rs) for a complete bounded loop.

## Validated client objects and pack entries

`client::decode_loose(compressed, expected_id, staging, window, limits)` validates
one complete loose response. Its staging capacity and inflation limits include
the Git header and body. It returns a borrowed `Object` only after the zlib
checksum, canonical type/unsigned decimal size, exact body length and expected
hash in the ID's format match. Concatenated streams and trailing bytes fail. `fetch_loose_object`
combines the existing dumb transport request with this decoding, using separate
compressed/inflated buffers. `Object::from_loose` performs envelope/ID checks on
bytes already inflated by a host. These checks do not validate commit/tree/tag
semantics or traverse the repository graph.

`pack::Reader::new(pack)` checks a complete SHA-1 pack's header and trailer.
Use `Reader::new_with_format(pack, ObjectFormat::Sha256)` and
`pack::validate_with_format` for SHA-256 packs.
`read_entry(staging, window, limits)` decodes one entry into caller buffers,
checks its declared size and records offsets and the wire CRC-32. A failure
preserves the cursor for retry; discard modified staging bytes. `finish()` or a
read returning `None` verifies the declared entry count and exact pack closure.
`Entry::verify_crc32` compares a trusted index CRC with the computed CRC; CRCs
are carried by Git's index files rather than the pack entries themselves.

Plain entries expose `Entry::object()` as a `ResolvedObject` with a computed
canonical ID. For deltas, provide a materialized base to `resolve_delta` plus
separate result storage and `DeltaLimits { max_output, max_instructions,
max_depth }`. Offset bases must have the correct offset in the same pack;
reference bases must have the advertised ID. Host objects can become reference
bases through `ResolvedObject::external` (SHA-1) or `external_with_format`.
Delta copies, inserts, base/result
sizes, exact instruction closure and chain depth are checked before returning
a canonical body and ID. `verify_id` compares it with a trusted expected ID.
`pack::apply_delta` provides the one-level instruction primitive separately.

The reader needs the complete compressed pack plus staging for one inflated
body/instruction stream and one 32 KiB history window. Delta application also
needs its canonical base and caller-bounded result storage. No recursion or
allocation occurs. The native `fetch` command retains its wire-output behavior.

`pack::materialize(pack, index, arena, scratch, window, limits)` resolves a
complete non-thin SHA-1 pack, including forward reference deltas, into a
`MaterializedPack`. The host supplies metadata slots and a byte arena; neither
the compressed pack nor scratch is retained. Lookup uses the sorted object ID
index. Inflation, delta depth/instructions, pass count and decoded bytes per
pass, delta-base metadata probes, and total tree-validation reads are bounded by
`MaterializeLimits`. Index sorting uses an iterative heap sort with constant
stack space. Failed imports invalidate partial storage.
This checks pack/object/tree structure; graph closure is a separate operation.

`object::Commit::parse` exposes the root tree and exact message bytes.
`object::Tree::parse` checks modes, byte-string names, ordering and duplicates,
then exposes borrowed entries. `Tree::parse_with_budget` bounds reads and alias
rescans; `Tree::parse` uses a 64 MiB validation budget. `object::Tag::parse` extracts an annotated tag's
target and declared kind. Commit and tree parsers default to SHA-1; their
`*_with_format` methods accept SHA-256, and `Tree::parse_with_budget_and_format`
combines the format with a caller-owned read budget. Tags infer their target ID
format from its hexadecimal width. Host filesystem limits stay in the adapters; use
`ValidatedRepository` for stronger graph and reference validation.

`client::SmartAdvertisement::parse` and `read_smart_reference` handle a downloaded
protocol-v0 advertisement without I/O. Its `fetch_options` selects advertised
capabilities and `validate_want` checks permission to request an explicit ID.
`write_fetch_request` deduplicates wanted IDs and encodes optional shallow/offset-delta negotiation.
SHA-256 wants include their object-format capability; mixed-format wants are
rejected before emitting output. `write_fetch_request_iter` accepts a cloneable
iterator so adapters can retain their own ID representation.
`fetch_pack_with_options` combines this with the host transport, and
`read_pack_response` handles shallow negotiation before authenticating a raw
pack; `read_pack_response_with_format` checks SHA-256 boundaries and checksums.
All these APIs use caller-owned storage without `std`, `alloc` or unsafe
code.

## Workspace

| Location | Responsibility |
| --- | --- |
| `src/` | `git-zero` facade with optional client/server reexports |
| `crates/git-zero-core/` | Objects, repositories, packet lines; `read` and `write` features |
| `crates/git-zero-client/` | Transport-independent HTTP discovery and fetching |
| `crates/git-zero-server/` | Smart/dumb HTTP repository routing |
| `crates/git-zero-rustix/` | `no_std` file/socket adapters; small `std` process wrapper |
| `examples/cloudflare-worker/wasm/` | `no_std` raw Wasm ABI with fixed buffers |
| `examples/cloudflare-worker/src/` | JavaScript host for Cloudflare metadata and HTTP |
| `examples/linux-kernel/` | Kernel VFS/socket adapter and `no_std` Rust codec for mounting Git snapshots |
| `tests/` | Core checks, real Git pack validation, parallel HTTP matrix |

The core and native adapter libraries never enable `std` or `alloc`. The native
CLI uses `std` for startup arguments, stdout, and diagnostics. JavaScript and the
test harnesses use their host runtimes' memory management; the no-allocation
guarantee applies to Rust core/adapter operations, not the surrounding platform.

## Crates and features

Use `git-zero` for the combined API, or depend on `git-zero-core`,
`git-zero-client` and `git-zero-server` directly. All library crates are safe
`no_std` Rust without `alloc`. Their default features preserve the existing API.

| Feature on `git-zero` | Enables |
| --- | --- |
| `read` | Zlib inflation, pack/delta decoding and repository validation |
| `write` | Snapshot creation, pack/loose/zlib encoding and pack caching |
| `client` | `git_zero::client`; enables `read` and the client crate |
| `server` | `git_zero::http` routing; enables `write` and the server crate |

`git-zero-core` has `read` and `write` features, both enabled by default.
Object IDs, borrowed object types, repository traits, object readers, packet lines,
I/O and HTTP request/response values are shared and always available. The client
crate depends on the core with `read`; the server crate uses `write`. A server
that validates custom repositories or serves independent refs through
`serve_with_selection` should also enable `read`. The facade forwards this
feature to the server crate when it is selected. `write` creates
and encodes objects; Git receive-pack/push remains future work.

```toml
[dependencies]
# Fetch and decode objects without server routing or object encoding.
git-zero = { version = "0.1.0", default-features = false, features = ["client"] }
```

For server-only use, select `features = ["server"]`; add `"read"` for
`ValidatedRepository`. For offline decoding/encoding, select `"read"`/`"write"`
on the facade or core. `git-zero-rustix` separately exposes `client`, `server`,
`storage` and `cli`; all are enabled by default. The CLI requires all adapters
and repository validation. The Worker example selects only `read` and `server`.

All five public crates share a release version. See [RELEASING.md](RELEASING.md)
for Runnerless release-please and crates.io publishing.

## Native demo

For an entirely kernel-side client, see the
[Linux filesystem module example](examples/linux-kernel/README.md). It mounts
a pinned Git commit from GitHub over handshake-only `no_std` HTTPS, or from an
ordinary HTTP origin, with bounded object caching, negative lookups and the
Linux page cache. The default refs view exposes each branch's message and file
contents at `main/msg` and `main/tree/src/lib.rs`. Select `checkout` for HEAD's
files at the mount root, or `ref=main` for a named branch. Both layouts support
Git symlinks; ref selection and checkout traversal live in the shared crates.
No userspace mount helper is needed.

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

The native library also provides `FileObject` and `FileRepository`. Supply an
ordered manifest of IDs/kinds and immutable files containing **raw uncompressed
object bodies**; this does not open an ordinary Git object directory or decode
loose zlib files/compressed pack entries. `FileObject::open_at` takes a borrowed
`CStr` path and a directory descriptor without allocating. It opens a regular
file with close-on-exec and rejects a symlink in the final path component; parent
directories and absolute paths retain ordinary `openat` behavior. `from_fd`
accepts an owned descriptor, and `from_range` accepts a raw body range within a
larger file. Ranges and reads use 64-bit offsets up to native `i64::MAX`.

```rust
use git_zero::{ObjectKind, Repository, ValidatedRepository};
use git_zero_rustix::{FileObject, FileRepository};

// dir, commit_id, tree_id and blob_id come from the host's object manifest.
let objects = [
    FileObject::open_at(&dir, c"commit", ObjectKind::Commit, commit_id)?,
    FileObject::open_at(&dir, c"tree", ObjectKind::Tree, tree_id)?,
    FileObject::open_at(&dir, c"blob", ObjectKind::Blob, blob_id)?,
];
let repository = FileRepository::new(commit_id, &objects);
let mut metadata = [None; 3];
let mut by_id = [0; 3];
let repository = ValidatedRepository::new(
    &repository, &mut metadata, &mut by_id, &mut [0; 1024], &mut [0; 512],
)?;
```

The caller owns the object array, and each object owns its descriptor. Enumeration
uses manifest order, and bodies are read with `pread`, so independent cursors and
threads do not alter a shared descriptor's file position. Replacement, rename or
unlink cannot redirect an open object to another inode. Hosts must prevent
in-place writes for the lifetime of the repository URL; an open descriptor alone
does not make an immutable snapshot. Invalid ranges/nonregular files return
`InvalidObject`, absent paths/indices return `ObjectNotFound`, and syscall failures
return `Backend`. Unexpected EOF after truncation returns `InvalidObject`.

`FileRepository::with_refs` configures sorted commit aliases. Construction trusts
the manifest's IDs and graph; `ValidatedRepository` checks them with bounded
scratch. To reuse a pack, call `with_prepacked(pack_fd, object_scratch,
compare_scratch)` on the file repository. It compares the complete file against
the core encoder's exact output, checking object closure/order, framing and
checksum without a pack-sized buffer. Corrupt, truncated, extra, or mismatched
pack bytes return `CorruptPack`; empty scratch returns `BufferTooSmall`.
The verified pack file must also remain immutable. This supports packs generated
by this encoder; importing arbitrary compressed/delta packs remains roadmap work.

`write_pack_with_scratch` reads the verified pack when attached and otherwise
encodes the raw object files. `FileRepository::serve_with_scratch` reuses that
pack for valid smart upload requests; dumb loose-object routes encode raw bodies.
To route through a validated index, pass its repository to
`http::serve_with_pack` and supply a closure calling the file repository's pack
writer. The closure runs once only when a valid upload request ends in `done`,
after ACK/NAK negotiation. Its `PackOutput` sink applies negotiated sideband
framing; the closure writes ordinary complete pack bytes.
Scratch and output limits remain explicit; discard partial output on any error.

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
Names must fit smart discovery packets. `repository::ref_name_limit(format)`,
`head_ref_limit(format)` and `unborn_ref_limit(format)` account for ID width,
symref, object-format and upload capabilities; peeled rows reserve three more bytes.
The `MAX_REF_NAME_LENGTH` and HEAD/unborn/peeled constants give the SHA-1 limits.
Changing refs leaves object IDs and pack-cache keys stable.
`http::Request::path` is a URL path: percent-encode reserved bytes in ref URLs.
The server decodes escapes once when matching ref names, preserving literal `+`.

Git checkout does not create directories for empty trees. Git's dumb HTTP walker
can also omit the empty-tree object from disk (observed with Git 2.53), causing
`fsck --strict` to report it missing. The loose-object endpoint serves it; clients
needing a complete disk object store must explicitly fetch that object or use
smart HTTP. See [tests/README.md](tests/README.md) for the interoperability check.

A custom `Repository` implementation must expose a stable, complete object set
reachable from HEAD and its refs. A resolved HEAD and branch refs must name
commits; other refs may name commits, trees, blobs or tags. An unborn HEAD can
coexist with other refs and their objects.
Bodies can be borrowed slices or generated/read from storage by offset.

`Repository::head_state` distinguishes `Head::Symbolic { target, id }`,
`Head::Detached(id)` and `Head::Unborn(target)`. Existing implementations default
to a symbolic HEAD using `head_ref()` and `head()`. For an unborn HEAD, `head()`
returns the all-zero sentinel; use `head_state().id()` to distinguish absence.
`EmptyRepository::new("refs/heads/main")` validates the target and serves an
empty ref list, the v0 `capabilities^{}` marker, and symbolic dumb HTTP `HEAD`.
This empty repository rejects every want, including the zero ID, before writing
output. An unborn repository with other refs advertises those refs without HEAD
or the empty-repository marker. V0 does not negotiate
an unborn default branch: Git's smart empty clone may use its own initial branch,
matching the reference server. Dumb `HEAD` preserves the configured target.

For a detached file-backed repository, use
`FileRepository::with_detached_head(&refs)`; `refs` may be empty. HTTP `HEAD`
returns the direct commit ID, and smart discovery omits the HEAD symref.
`with_refs` switches back to a symbolic HEAD. Indexing and graph validation
preserve all three HEAD states.

`RefRepository::new(&provider, head, &references)` overlays caller-owned ref and
HEAD metadata on any stable object provider. Each `Reference` carries a resolved
ID, an optional raw `symbolic_target`, and an optional final non-tag `peeled` ID.
Use `Reference::direct(name, id)` for ordinary branches or lightweight tags.
Symbolic chains must resolve to an existing ref with the same ID. A symbolic HEAD
can follow branch aliases to a lightweight commit tag. Ref names must
be sorted, unique and free of file/directory conflicts. Peeled names reserve
three extra packet bytes for `^{}`. `ValidatedRepository` checks annotated tags,
including nested tags and tree/blob targets, against the supplied final peel.
The default `Repository` ref methods preserve legacy HEAD aliases; wrappers must
forward the new ID, symbolic-target and peeled metadata.

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
The native demo validates and indexes its repository once before accepting requests.

Use `ValidatedRepository` to check custom providers and cache their IDs and
64-bit metadata, including providers that only support streamed bodies:

```rust
use git_zero::ValidatedRepository;

let mut objects = [None; 16];
let mut by_id = [0; 16];
let mut read = [0; 1024];
let mut parse = [0; 512];
let repository = ValidatedRepository::new(
    &repository, &mut objects, &mut by_id, &mut read, &mut parse,
)?;
```

Both arrays need one slot per enumerated object. Empty repositories need no
scratch. For nonempty bodies, read scratch needs at least one byte; parse scratch
must fit each commit/tag header line without its LF and
twice each tree name without its NUL. Object bodies can exceed both buffers.
The scratch buffers are reusable after construction; only the arrays and provider
remain borrowed. `metadata` exposes each validated ID, kind and 64-bit size;
ID lookup uses binary search. The wrapper preserves enumeration, refs and wire
bytes, and delegates body reads to the provider, which must remain stable.

Validation hashes every body and rejects mismatched IDs, duplicate object IDs,
missing links, incorrect target kinds, malformed commit headers/tree records,
noncanonical tree modes, invalid names, unsorted/duplicate tree names, and extra
objects unreachable from every advertised root and HEAD. It follows parent
commits, tags and shared subtrees
without recursion. Gitlinks (`160000`) name external commits and are not followed.
This checks the repository graph contract; identity
semantics, platform-specific filename aliases and special-file policies such as
`.gitmodules` need separate validation to match every Git `fsck` check.

Insufficient arrays or scratch return `BufferTooSmall`; missing objects/links
return `ObjectNotFound`; invalid refs/names/order use `InvalidReference`,
`InvalidFileName` and `UnsortedFiles`. Other invalid graph data returns
`InvalidObject`, and backend errors propagate. Discard partial arrays on failure.
Hashing is linear in body bytes. Pending-object scans and directory/file duplicate
checks can take quadratic work; the latter reread earlier tree entries to avoid
an unbounded name set. Validation does not use the provider's `find_index`, so a
bad lookup implementation cannot hide missing or mismatched graph links.

For independent refs, use `http::serve_with_selection` with the validated provider:

```rust
use git_zero::{ObjectId, http};

let mut wants = [ObjectId::Sha1([0; 20]); 8];
let mut objects = [None; 64];
let mut indices = [0; 64];
let mut read = [0; 1024];
let mut parse = [0; 512];
let mut storage = http::SelectionStorage {
    wants: &mut wants, objects: &mut objects, indices: &mut indices,
    read: &mut read, parse: &mut parse, graph_read_limit: 1 << 20,
};
let response = http::serve_with_selection(&repository, request, &mut storage, &mut output)?;
```

`http::SelectionBuffers::<16, 128, 1024, 128>::new()` is an optional owner for
want, object, read and parse buffers. Its `storage(graph_read_limit)` method lends
the same descriptor used above; capacity and byte-footprint queries support host
budget reporting. Separate owners keep overlapping responses independent.

Both selection arrays need one slot per provider object. Wants need one slot per
distinct requested ID. Scratch follows validation's header/tree-name limits;
`graph_read_limit` caps actual structural read bytes, including buffered reads and
rereads. Selection uses no body-sized buffer or allocation. Only advertised HEAD,
ref and peeled IDs are accepted; unknown or merely reachable IDs are rejected.
Duplicate and overlapping wants produce each object once. Ordinary requests without `done`
negotiate ACK/NAK without graph traversal. Selection failures occur before output,
and the same storage can be reused on retry. Known commit haves omit their complete
closure, sharing `graph_read_limit` across common and wanted traversals. Unknown
and non-commit haves are ignored. Requests replay wants and common haves each
round; no connection state or have-ID array is retained. A client's shallow
boundaries stop common-history traversal, so missing parents are never excluded.

V0 advertisements include `multi_ack`, `multi_ack_detailed`, `side-band`,
`side-band-64k` and `no-progress`. A client chooses at most one ACK mode and one
sideband mode on the first want. Without an ACK capability, the first common
commit gets one ACK. Multi modes acknowledge common commits and finish a no-done
round with NAK; a done round ends negotiation with a final ACK (or NAK if none
are common), then the pack. No `ready`/`no-done` shortcuts are advertised.
Sideband sends channel-1 pack bytes through a fixed 1 KiB buffer and a terminal
flush; no progress messages are emitted. Unsupported, conflicting or duplicated
capabilities, misplaced wants/haves and incomplete requests fail before output.
V0 `include-tag` remains unadvertised.

`Request` carries `content_type` and `git_protocol` from the HTTP host. A missing
protocol header selects v0; `version=1` adds its version announcement and
`version=2` selects ls-refs/fetch. V2 supports symrefs, peels, prefix selection,
unborn HEAD and include-tag. Done fetches send a sideband packfile section without
ACKs; negotiation rounds return ACKs or NAK and wait for done. Thin-pack/ofs-delta
hints permit full non-delta packs. Selection-enabled discovery adds v0/v1
`shallow filter` or v2 `fetch=shallow filter`. Legacy complete-pack APIs advertise
their smaller capability set and reject extensions. The client primitives continue using v0.

Selected fetches support absolute `deepen N` (1..=2147483647), client `shallow`
IDs, and `filter blob:none` / `filter blob:limit=N` (optional lowercase k/m/g
binary suffixes). Depth counts commits from every wanted tip, following the
shortest merge path; annotated tags preserve their target's depth. Depth requests
resend the bounded closure rather than trusting shallow haves. Responses update
shallow/unshallow boundaries, including root commits exactly at the depth limit;
`git fetch --depth=N` and `--unshallow` work with the same arrays and graph budget.
V0's initial shallow-list probe has no ACK/NAK; v2 uses a shallow-info section.

Blob filters omit bodies of the selected size or larger and retain explicitly
wanted blobs. Partial clones currently require `--no-checkout`: the advertised-ID
want policy still rejects requests for unadvertised promised blobs, so lazy
checkout retrieval remains deferred. Relative/time/exclusion deepening
(`--deepen`, deepen-since/not), tree/sparse/combine filters, unknown/non-commit
shallow IDs, malformed/duplicate options and exhausted budgets fail before
output. Shallow probes also traverse within the graph budget; hosts must size
control storage for their boundary packets. No history or filter state persists
between requests.

Upload requests require the Git upload request media type. Known routes enforce
methods and service/version selection, reject duplicate selectors and unsafe or
malformed paths, and accept at most one leading slash. Native and Worker hosts
decode gzip uploads with bounded output; native compressed plus expanded bytes
must fit the caller's request storage, with a fixed 32 KiB stack history window.
Chunked requests and incremental HTTP I/O remain planned.

On a failed buffered upload, discard the complete failed body. A host can call
`http::write_upload_error(error, output)` on an empty sink to produce a terminal
`ERR` packet with a bounded public message, and choose its HTTP status. Never
append it to a partial raw pack. Streaming hosts must close failed transfers.

`ValidatedRepository::select_objects` also accepts any present root for host use;
`select_missing_objects` subtracts explicit complete common closures with the
same array and read budgets. `select_bounded_fetch` adds `FetchOptions`, client
shallow IDs, and per-object shallow/unshallow markers in the existing work array.
`pack::write_selected_with_scratch` emits the resulting unique object indices.
The legacy `http::serve`, `serve_with_scratch`, `serve_with_pack` and HEAD-key
`cache::write` support complete HEAD closures whose refs all alias HEAD.
Complete-pack hooks still negotiate and frame sideband, but may resend common
objects. Use `serve_with_selection` to omit them.
Independent roots require selection; legacy upload-pack/cache paths return
`Unsupported` before invoking pack/cache backends. Requested-closure cache keys
and prepacked reuse remain roadmap work.

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

`ObjectReader` stores its cursor and metadata inline; `ObjectHasher` adds hash
state. `core::mem::size_of` gives their exact state budgets for a target. Neither
owns scratch or allocates. A read/hash step performs at most one provider read;
EOF needs no buffer, and nonempty bodies need at least one scratch byte. Early EOF
and overreported read counts return `InvalidObject`; backend errors propagate.
Read errors leave the cursor/hash unchanged for retry. Providers must report exact
lengths and stable contents; use `ValidatedRepository` for object-graph checks.
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

`cargo test` runs the eight-way native interoperability matrix plus sixteen
upstream-inspired differential cases in parallel, defaulting to the CPU count. All
HTTP tests need permission to bind/connect to loopback. The Worker tests clone
through the actual Wasm module with real Git over both HTTP protocols. See
[tests/README.md](tests/README.md) for selective runs, CI sharding, and the optional
Git upstream test runner. CI also checks the core against a bare-metal target,
which has neither `std` nor an allocator.

Protocol references: [Git HTTP](https://git-scm.com/docs/http-protocol),
[Git pack format](https://git-scm.com/docs/pack-format),
[Cloudflare Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/).

Licensed under MIT OR Apache-2.0.
