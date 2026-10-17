# Crates and usage

Run shell examples from the repository root unless shown otherwise.

## Crates and features

Use `git-zero` for the combined API, or depend on `git-zero-core`,
`git-zero-client` and `git-zero-server` directly. All library crates are safe
`no_std` Rust without `alloc`. Their default features preserve the existing API.

| Feature on `git-zero` | Enables |
| --- | --- |
| `read` | Zlib inflation, pack/delta decoding and repository validation |
| `write` | Snapshot creation, pack/loose/zlib encoding and pack caching |
| `client` | `git_zero::client`; enables `read` and the client crate |
| `server` | `git_zero::http` routing; enables `write` and the server crate |

`git-zero-core` has `read` and `write` features, both enabled by default.
Object IDs, borrowed object types, repository traits, object readers, packet lines,
I/O and HTTP request/response values are shared and always available. The client
crate depends on the core with `read`; the server crate uses `write`. A server
that validates custom repositories or serves independent refs through
`serve_with_selection` should also enable `read`. The facade forwards this
feature to the server crate when it is selected. `write` creates
and encodes objects; Git receive-pack/push remains future work.

```toml
[dependencies]
# Fetch and decode objects without server routing or object encoding.
git-zero = { version = "0.1.0", default-features = false, features = ["client"] }
```

For server-only use, select `features = ["server"]`; add `"read"` for
`ValidatedRepository`. For offline decoding/encoding, select `"read"`/`"write"`
on the facade or core. `git-zero-rustix` separately exposes `client`, `server`,
`storage` and `cli`; all are enabled by default. The CLI requires all adapters
and repository validation. The Worker example selects only `read` and `server`.

All five public crates share a release version. See the
[crate and release guidance](../CONTRIBUTING.md#crate-boundaries-and-releases)
for package boundaries and publishing checks.

## Native demo

For an entirely kernel-side client, see the
[Linux filesystem module example](../examples/linux-kernel/README.md). It mounts
a pinned Git commit from GitHub over handshake-only `no_std` HTTPS, or from an
ordinary HTTP origin, with bounded object caching, negative lookups and the
Linux page cache. The default refs view exposes each branch's message and file
contents at `main/msg` and `main/tree/src/lib.rs`. Select `checkout` for HEAD's
files at the mount root, or `ref=main` for a named branch. Both layouts support
Git symlinks; ref selection and checkout traversal live in the shared crates.
No userspace mount helper is needed.

Use Rust 1.85 or newer, Git with HTTP support, and Python 3 for tests.

```sh
cargo run -p git-zero-rustix --bin git-zero -- serve 8080
```

In another terminal:

```sh
git clone http://127.0.0.1:8080/repo.git demo-smart
GIT_SMART_HTTP=0 git clone http://127.0.0.1:8080/repo.git demo-dumb
cargo run -p git-zero-rustix --bin git-zero -- ls-remote http://127.0.0.1:8080/repo.git
cargo run -q -p git-zero-rustix --bin git-zero -- fetch http://127.0.0.1:8080/repo.git > snapshot.pack
```

The server prints its URL after binding; port `0` selects an OS-assigned port.
It serves a fixed `README.md` snapshot and handles connections serially. The
native client accepts `http://IPv4:port/path` URLs. Its current transport uses
HTTP/1.0 with connection close, bounded buffers, and socket timeouts; DNS, TLS,
redirects, and chunked responses need a richer transport.

The native library also provides `FileObject` and `FileRepository`. Supply an
ordered manifest of IDs/kinds and immutable files containing **raw uncompressed
object bodies**; this does not open an ordinary Git object directory or decode
loose zlib files/compressed pack entries. `FileObject::open_at` takes a borrowed
`CStr` path and a directory descriptor without allocating. It opens a regular
file with close-on-exec and rejects a symlink in the final path component; parent
directories and absolute paths retain ordinary `openat` behavior. `from_fd`
accepts an owned descriptor, and `from_range` accepts a raw body range within a
larger file. Ranges and reads use 64-bit offsets up to native `i64::MAX`.

```rust
use git_zero::{ObjectKind, Repository, ValidatedRepository};
use git_zero_rustix::{FileObject, FileRepository};

// dir, commit_id, tree_id and blob_id come from the host's object manifest.
let objects = [
    FileObject::open_at(&dir, c"commit", ObjectKind::Commit, commit_id)?,
    FileObject::open_at(&dir, c"tree", ObjectKind::Tree, tree_id)?,
    FileObject::open_at(&dir, c"blob", ObjectKind::Blob, blob_id)?,
];
let repository = FileRepository::new(commit_id, &objects);
let mut metadata = [None; 3];
let mut by_id = [0; 3];
let repository = ValidatedRepository::new(
    &repository, &mut metadata, &mut by_id, &mut [0; 1024], &mut [0; 512],
)?;
```

The caller owns the object array, and each object owns its descriptor. Enumeration
uses manifest order, and bodies are read with `pread`, so independent cursors and
threads do not alter a shared descriptor's file position. Replacement, rename or
unlink cannot redirect an open object to another inode. Hosts must prevent
in-place writes for the lifetime of the repository URL; an open descriptor alone
does not make an immutable snapshot. Invalid ranges/nonregular files return
`InvalidObject`, absent paths/indices return `ObjectNotFound`, and syscall failures
return `Backend`. Unexpected EOF after truncation returns `InvalidObject`.

`FileRepository::with_refs` configures sorted commit aliases. Construction trusts
the manifest's IDs and graph; `ValidatedRepository` checks them with bounded
scratch. To reuse a pack, call `with_prepacked(pack_fd, object_scratch,
compare_scratch)` on the file repository. It compares the complete file against
the core encoder's exact output, checking object closure/order, framing and
checksum without a pack-sized buffer. Corrupt, truncated, extra, or mismatched
pack bytes return `CorruptPack`; empty scratch returns `BufferTooSmall`.
The verified pack file must also remain immutable. This supports packs generated
by this encoder; importing arbitrary compressed/delta packs remains roadmap work.

`write_pack_with_scratch` reads the verified pack when attached and otherwise
encodes the raw object files. `FileRepository::serve_with_scratch` reuses that
pack for valid smart upload requests; dumb loose-object routes encode raw bodies.
To route through a validated index, pass its repository to
`http::serve_with_pack` and supply a closure calling the file repository's pack
writer. The closure runs once only when a valid upload request ends in `done`,
after ACK/NAK negotiation. Its `PackOutput` sink applies negotiated sideband
framing; the closure writes ordinary complete pack bytes.
Scratch and output limits remain explicit; discard partial output on any error.

## Cloudflare Workers demo

See [the demo README](../examples/cloudflare-worker/README.md) for setup and its
snapshot URL format. It exports available `request.cf` properties, explicitly
selected public `cf-*` request headers and environment variables, and a runtime descriptor
as four JSON files in one root commit. Internal platform state unavailable to
Workers cannot be exported. Environment bindings use an explicit allowlist
because the runtime does not distinguish string variables from secrets.

```sh
rustup target add wasm32-unknown-unknown
cd examples/cloudflare-worker
npm ci
npm run dev
```

Open the local Worker URL to obtain `clone_url`, then pass that URL to `git clone`.
The default URL contains the captured public metadata, so every request and
isolate can reconstruct the same snapshot without a persistence service. The
demo also supports optional KV/R2 persistence for shorter immutable snapshot
URLs; see its README for configuration and limits.
