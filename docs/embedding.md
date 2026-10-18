# Embed the core

## Create snapshots

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
    content_type: None,
    git_protocol: None,
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
smart HTTP. See [tests/README.md](../tests/README.md) for the interoperability check.

## Supply repositories and refs

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

## Index and validate repositories

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

## Select fetch closures

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

## Stream object bodies and packs

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
