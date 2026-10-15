# git-zero-server

Allocation-free Git smart and dumb HTTP server routing. Safe `no_std` Rust without `alloc`.

Smart HTTP discovery/upload-pack and dumb HTTP refs/loose-object routing.
The host supplies repository data, response output and HTTP framing.
Depends on `git-zero-core` with only `write` enabled.

Part of [git-zero](https://github.com/no-std-rs/git-zero).

Enable `read` for `SelectionStorage` and `serve_with_selection`, which select
bounded reachable packs from a validated repository with independent refs.

V0 supports single ACK, `multi_ack`, `multi_ack_detailed`, `side-band`,
`side-band-64k` and `no-progress` in SHA-1/SHA-256. Requests replay wants and
common commit haves each round. Selection excludes their complete reachable
history within one graph-read budget; complete-pack hooks can resend it.
The hook's `PackOutput` sink frames ordinary pack bytes with a fixed 1 KiB buffer.
No progress messages or `ready`/`no-done` shortcuts are emitted.

Parse and selection failures precede output. Any later error invalidates the
whole response. Buffered hosts may replace it with `write_upload_error` on an
empty sink; streaming hosts close failed transfers. Shallow/filter requests,
include-tag and protocol v1/v2 remain unadvertised. Hosts supply HTTP validation,
authentication and request/output capacities.
