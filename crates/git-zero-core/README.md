# git-zero-core

Allocation-free Git objects, repositories, pack and zlib primitives. Safe `no_std` Rust without `alloc`.

`read` enables zlib inflation, pack/delta decoding and repository validation.
`write` enables snapshots, pack/zlib encoding and pack caching. Both are enabled
by default. Object IDs, borrowed objects, repositories, readers, packet lines and
HTTP request/response values are always available.

Part of [git-zero](https://github.com/no-std-rs/git-zero).
