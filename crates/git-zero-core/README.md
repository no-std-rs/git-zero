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
