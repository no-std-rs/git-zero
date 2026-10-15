# git-zero-core

Allocation-free Git objects, repositories, pack and zlib primitives. Safe `no_std` Rust without `alloc`.

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
