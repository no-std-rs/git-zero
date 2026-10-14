# git-zero-client

Allocation-free transport-independent Git HTTP client. Safe `no_std` Rust without `alloc`.

Discovery, HEAD resolution, smart pack fetching and validated dumb loose-object
fetching. The host supplies transport, buffers and limits through `Transport`.
Depends on `git-zero-core` with only `read` enabled.

Part of [git-zero](https://github.com/no-std-rs/git-zero).
