# git-zero-core

`pack::write_indexed_with_scratch` streams a stored-block pack and a Git v2 index
using one caller-owned `pack::IndexEntry` per object and bounded read scratch.
It supports SHA-1/SHA-256, selected object orders, wire CRCs and 64-bit offsets.
Publish the pair only after success: errors invalidate both outputs. Existing
verified entry metadata can be streamed separately with `pack::write_index`.

Allocation-free Git objects, repositories, pack and zlib primitives. Safe `no_std` Rust without `alloc`.

With `read`, `pack::Decoder` accepts fragmented SHA-1/SHA-256 packs without
retaining a complete pack, object body, or object-count-sized index. The host
supplies a 32 KiB inflate window and arbitrary input/output slices. Events expose
entry headers, body fragments, entry CRCs, plain-object IDs, and the verified
pack checksum. Offset/reference deltas emit instruction bytes for separate base
resolution. Wire size, object count, inflation size and expansion ratio have
independent limits. Stage all output privately until the trailer and subsequent
object/base/connectivity validation pass; this decoder alone is not a push
validator. No Git executable, filesystem, allocator or runtime API is used.

`read` enables zlib inflation, pack/delta decoding and repository validation.
`write` enables snapshots, pack/zlib encoding and pack caching. Both are enabled
by default. Object IDs, borrowed objects, repositories, readers, packet lines and
HTTP request/response values are always available.

With `read`, `checkout` provides borrowed file/symlink contents, byte-name tree
lookup and bounded tag/commit-to-tree traversal over a caller-owned `ObjectSource`.
It supports both object formats and retains only IDs and offsets between source
reads, so caches may evict the previous object on every lookup. Hosts supply
hash-verified objects and implement permissions, filesystem limits and I/O.

Part of [git-zero](https://github.com/no-std-rs/git-zero).

With `write`, `pack::Encoder` and `pack::LooseEncoder` expose resumable pulls into
caller-owned output slices. They retain stored-DEFLATE framing, Adler-32 and pack
hash state without object-sized scratch. Packs accept a borrowed selection or all
objects. Hold repository metadata, bodies and selected indices stable until EOF;
a host can recreate the repository from immutable inputs between pulls. Each call
performs at most one body read. Preserve unsent output before advancing again.
Encoding matches the synchronous stored-block writers in both object formats.

`cache::selected` caches raw stored-block packs by their exact object set. It
supports independent refs and partial selections in SHA-1/SHA-256. Complete
authorization, visible-ref/reachability checks, graph selection and traversal
budgets before every call. Pass strictly ascending selected indices with
`Selection::Indices` (or `Selection::All` for all objects). ACK-only rounds and shallow-list probes skip the cache; their
control packets are generated separately.

```rust
# use git_zero_core::{cache::selected::{self, Buffers, FixedCache, Selection}, ObjectId, Repository, io::Write, Result};
# fn example(repository: &impl Repository, indices: &[usize], first_response: &mut impl Write, second_response: &mut impl Write) -> Result<()> {
fn serve(
    repository: &impl Repository,
    indices: &[usize],
    cache: &mut FixedCache<'_>,
    output: &mut impl Write,
) -> Result<()> {
    let mut ids = [ObjectId::Sha1([0; 20]); 64]; // One slot per selected object.
    let mut staging = [0; 8192];
    selected::write(repository, Selection::Indices(indices), cache, Buffers {
        ids: &mut ids,
        pack: &mut staging,
        read: &mut [0; 1024],
        byte_limit: 8192,
    }, output)?;
    Ok(())
}

// The host retains this cache and supplies a fresh response sink for each call.
let mut cache_bytes = [0; 8192];
let mut cache = FixedCache::new(&mut cache_bytes);
serve(repository, indices, &mut cache, first_response)?;
serve(repository, indices, &mut cache, second_response)?;
# assert_eq!(cache.statistics().hits, 1);
# Ok(())
# }
```

Hits require ID scratch and a byte limit, with no pack staging or body reads
when the provider supplies precomputed IDs. Both hits and misses validate the
pack trailer, stored zlib framing/Adler32, canonical object hashes and exact set
before output; misses also validate before insertion. The key's 48-byte encoding
includes schema, object format, codec and encoder revision. Equivalent sets can
use different pack orders: publish pack/index URLs using the actual pack
checksum, rather than the selected-set key digest. Backend errors and
corruption propagate. Failed insertion preserves the old entry; an output error
may retain a new complete entry for retry. Hosts discard partial output and own
atomic publication, persistence and coordination between concurrent requests.
