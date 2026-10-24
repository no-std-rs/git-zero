# git-zero-client

Allocation-free transport-independent Git HTTP client. Safe `no_std` Rust without `alloc`.

```toml
[dependencies]
git-zero-client = "0.1.0"
```

See the [API reference](https://docs.rs/git-zero-client) for `Transport`, fetch
options, and buffer contracts. This crate supplies protocol behavior; the host
supplies network I/O and any TLS, DNS, authentication, or runtime integration.

Discovery, HEAD resolution, smart pack fetching and validated dumb loose-object
fetching. The host supplies transport, buffers and limits through `Transport`.
Depends on `git-zero-core` with only `read` enabled.

`reference_name` normalizes short branch selectors, full refs and HEAD into
caller storage. `SmartAdvertisement::resolve_reference` selects an exact
advertised ref; `resolve_reference` follows ordinary-HTTP symbolic refs with
caller-supplied read and buffer limits, percent-encoding their paths. Checkout
object traversal and file/symlink contents live in `git_zero_core::checkout`.

Part of [git-zero](https://github.com/no-std-rs/git-zero).
