# Immutable packed dumb HTTP

`git_zero::http::dumb::Publication` publishes a complete stored-block pack and
matching Git v2 index without retaining either file in memory. It supports
SHA-1 and SHA-256 and exists with the server's write-only feature set.

Prepare a trusted immutable repository once, then dispatch publication routes
before its normal smart/loose handler:

```rust,ignore
let mut entries = [git_zero::pack::IndexEntry::EMPTY; OBJECT_CAPACITY];
let publication = git_zero::http::dumb::Publication::new(
    &repository,
    &mut entries,
    &mut scratch,
    git_zero::http::dumb::LooseObjects::Disabled,
)?;
if let Some(mut cursor) = publication.prepare(request)? {
    // Send cursor.response() and Content-Length: cursor.size().
    // Stage every successful read until all its bytes have reached the socket.
    loop {
        let count = cursor.read(&mut staging)?;
        if count == 0 { break; }
        transport.write_all(&staging[..count])?;
    }
} else {
    // Serve refs/HEAD/smart requests from the SAME immutable repository.
}
```

`Publication::serve` provides the equivalent buffered-writer interface, returning
`None` without output for fallthrough routes. Native `respond_published` writes
HTTP/1.0 framing and cancels its response on provider/socket errors. Each cursor
pins the repository; cancellation or output progress affects only that cursor.

The publication routes are:

| Route | Content |
| --- | --- |
| `objects/info/packs` | `P pack-<checksum>.pack`, followed by a blank line |
| `objects/pack/pack-<checksum>.pack` | Complete checksummed stored pack |
| `objects/pack/pack-<checksum>.idx` | Matching version-2 index |

The checksum is the pack trailer, independent of selected-cache keys. Wrong
names/suffixes return 404; method/version/service/path validation is shared with
the ordinary server. Pack/index streams have exact known lengths. `Disabled`
rejects loose-object routes; `Enabled` passes them to the normal handler. A mixed
namespace can expose a subset of loose objects plus the complete published pack.

Preparation encodes the graph once into counting sinks and retains one sorted
`IndexEntry` per object. Downloads replay the deterministic pack encoder; index
downloads read only metadata. `pack::IndexEncoder` also provides independent pull
indexes, including 64-bit offsets. Scratch/output buffers can be one byte; larger
buffers reduce transport overhead. This is bounded storage, not constant total
work: preparing a publication walks/encodes its complete graph.

Validate untrusted providers first. Keep HEAD, refs, enumeration, canonical IDs
and all bodies stable for the lifetime of a published URL, including retries.
Use a new immutable URL for a new generation. A name/checksum does not authorize
access; hosts choose the public projection and authenticate every request before
publication dispatch. Failed preparation publishes nothing. Discard buffered
failure output; abort a stream after headers rather than appending a success.

## Verified native files

With rustix `storage` and `server`, `FilePublication::from_fds` compares both
regular files byte-for-byte with a freshly generated publication. It checks pack
framing/checksum and index offsets, CRCs and trailers before making either file
servable. Extra/truncated/corrupt bytes fail. Arbitrary compressed/delta packs are
outside this adapter's contract.

The adapter pins owned descriptors and downloads via positional reads; it neither
reencodes nor reads raw object bodies during downloads. Use
`respond_file_published` for HTTP framing. Independent cursors do not share file
positions. Rename/unlink/replacement preserves pinned downloads; prevent in-place
writes for the publication's lifetime. Post-publication truncation fails the
cursor and requires closing the response.

The `packed-dumb` example imports the bounded raw export used by native tests,
writes a new private generation, verifies its files and only then starts serving:

```sh
python3 tests/server_fixture.py --repo /path/to/repo.git --root /tmp/raw-export
cargo run -p git-zero-rustix --example packed-dumb -- /tmp/raw-export /tmp/new-generation
# Use the printed immutable URL:
GIT_SMART_HTTP=0 git clone "$URL" /tmp/packed-clone
```

The generation directory must not already exist. Keep exported bodies and
published files immutable. The example owns one blocking connection at a time;
production quotas, crash cleanup, directory durability, TLS and distributed
locking remain host work.

## Bounded Worker imports

A `createRepositoryWorker` trusted loader may return `publication: "packed-only"`
or `"mixed"`; omission retains loose-only behavior. The imported graph is still
limited to 64 objects, eight refs and 128 KiB of raw bodies. Each response owns an
instance and a 64-entry metadata array; pack/index bytes stream through existing
fixed output storage. The host verifies required exports before reading a body.
The `stream_start_repository_packed` ABI uses `loose=0` for packed-only or `1`
for loose fallthrough; `publication_length` returns the active complete length.

This is synchronous bounded preparation of resident raw objects. It does not
provide asynchronous selected-pack KV/R2 caching or a generated/remote provider.
The metadata-capture demo retains its default loose layout.
