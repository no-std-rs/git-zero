# git-zero-server

Allocation-free Git smart and dumb HTTP server routing. Safe `no_std` Rust without `alloc`.

Smart HTTP discovery/upload-pack and dumb HTTP refs/loose-object routing.
The host supplies repository data, response output and HTTP framing.
Depends on `git-zero-core` with only `write` enabled.

Part of [git-zero](https://github.com/no-std-rs/git-zero).

Enable `read` for `SelectionStorage` and `serve_with_selection`, which select
bounded reachable packs from a validated repository with independent refs.
