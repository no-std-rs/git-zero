# Kernel Git filesystem example

> Not because we asked if we should, but because we could. :)

`gitfs.ko` mounts a Git commit as a read-only Linux filesystem. TCP sockets,
HTTP/HTTPS, ref resolution, object fetching, decompression, SHA-1 validation, tree
traversal, and caching all run **inside the module**. There is no daemon,
FUSE connection, subprocess, userspace upcall, or local checkout.

C implements VFS, kernel sockets, DNS and buffer ownership. A small Rust ABI links
TLS 1.3 and the existing allocation-free `git-zero` decoder and SHA-1 into the
module. Linux allocates the buffers; the kernel Rust build uses neither `std`
nor `alloc`.

## Build and mount

This example targets **x86-64 Linux 6.17**, tested on 6.17.7. It needs a
configured kernel development tree with `CONFIG_RUST=y`, `CONFIG_INET=y` and
`scripts/target.json`, a C compiler, binutils, Make, ripgrep, and a recent Rust
toolchain with `rust-src`. The tested Rust compiler is 1.97.1.

```sh
rustup component add rust-src
make -C examples/linux-kernel
# Or select another prepared kernel build:
make -C examples/linux-kernel KDIR=/path/to/kernel/build
```

The build rebuilds `core` and `compiler_builtins` for the kernel target ABI.
It disables SIMD, red zones and jump tables, enables configured return thunks
and IBT, and selects software SHA-1. `RUSTC_BOOTSTRAP=1` enables the unstable
compiler controls used for this build; a host userspace `libcore` is not linked.

Public GitHub repositories can be mounted directly:

```sh
sudo insmod examples/linux-kernel/gitfs.ko
sudo mkdir -p /mnt/git
sudo mount -t gitfs -o ro https://github.com/octocat/Hello-World /mnt/git
cat /mnt/git/README
sudo umount /mnt/git
```

HTTPS uses smart Git HTTP automatically. It resolves the hostname through a
kernel UDP DNS query, completes a TLS handshake, resolves HEAD, and downloads a
depth-one pack when the origin supports shallow fetch. Offset and reference deltas are resolved
into a bounded snapshot cache before the mount becomes visible. Reading the
mounted snapshot needs no further network access. The GitHub URL's `.git` suffix
is optional.

For a small origin, run the existing server in another terminal:

```sh
cargo run -p git-zero-rustix --bin git-zero -- serve 8080
```

Load and mount:

```sh
sudo insmod examples/linux-kernel/gitfs.ko
sudo mkdir -p /mnt/git
sudo mount -t gitfs -o ro,cache_mb=64,object_mb=8 \
  http://127.0.0.1:8080/repo.git /mnt/git
ls -la /mnt/git
cat /mnt/git/README.md
sudo umount /mnt/git
sudo rmmod gitfs
```

The server is an ordinary repository origin, with no special relationship to
the mount. A static HTTP server exposing a bare repository's loose objects also
works.

By default, the module resolves `HEAD` once and pins that commit. To select a
commit directly and skip ref requests:

```sh
sudo mount -t gitfs -o ro,commit=0123456789abcdef0123456789abcdef01234567 \
  http://192.0.2.10:8080/repo.git /mnt/git
```

Replace the example SHA-1 with an existing commit's full ID. The resolved ID
appears in `/proc/mounts`. Upstream branch updates do not change existing mounts;
unmount and mount again to see a new revision. Read-write remounts are rejected.

## Caching

Smart mounts retain the complete materialized snapshot and a sorted object-ID
index within `cache_mb`. The compressed pack is discarded after validation.
This avoids per-file requests and keeps the mounted tree available offline.
Oversized packs, objects, or snapshots fail the mount instead of exposing a
partial checkout.

Ordinary HTTP mounts have a hash table keyed by the complete object ID and a
byte-budgeted LRU. Objects enter it only after zlib, canonical envelope, length and expected
SHA-1 checks pass. Identical blobs share bytes across paths and file modes.

- A smart mount normally uses two HTTPS requests: discovery and upload-pack.
- Mounting ordinary-HTTP `HEAD` normally requests `HEAD`, its branch ref, the
  commit and the root tree. An explicit commit normally needs two requests.
- Directory listing fetches its tree, never child blobs. File lookup/stat fetches
  its blob once to learn its size; Git trees do not store file sizes.
- Missing names come from the directory tree and become negative dentries.
  Editor/build-tool probes do not request nonexistent objects while that tree
  is cached.
- Linux's page cache handles repeated reads, mmap and splice. The object cache
  supplies a reclaimed page without another HTTP request.
- A per-mount mutex covers fetch/insertion to prevent duplicate downloads.
  This small example also serializes unrelated misses.
- Failed HTTP/object validation results have a short retry TTL. Interruption
  and allocation failures are not cached.
- Eviction permits refetch. Successful entries have no TTL because objects are
  immutable and the commit is pinned. Cache data is released on unmount.

| Option | Default | Range / meaning |
| --- | --- | --- |
| `commit` | Resolve `HEAD` | Full SHA-1 of a commit, not an annotated tag |
| `cache_mb` | 64 | 1–1024 MiB, strictly larger than the object limit plus an entry |
| `object_mb` | 8 | 1–64 MiB maximum uncompressed object body |
| `timeout_ms` | 5000 | 1–60000 ms total deadline per HTTP request |
| `error_ttl_ms` | 5000 | 0–60000 ms; zero disables failure retention |
| `smart` | Automatic for HTTPS | Enable smart upload-pack for plain HTTP too |
| `pack_mb` | 64 | 1–1024 MiB maximum smart HTTP response |
| `dns` | `1.1.1.1` | IPv4 DNS server; a bounded UDP A query, without resolver upcalls |
| `addr` | Resolve hostname | Explicit IPv4 destination; TLS sends the URL hostname as SNI |

The LRU counts payload bytes and entry structs, excluding allocator overhead.
Fetching additionally uses bounded wire, staging and final-body buffers, plus
32 KiB of decoder history. The inode/dentry/page caches have their normal
independent accounting and reclamation. Unmount logs request/hit/miss/eviction
and retained-byte counters.

## Current scope

The transport supports Git's
[ordinary and smart HTTP protocols](https://git-scm.com/docs/http-protocol).
It accepts `http://` and `https://` URLs with IPv4 addresses or DNS names,
Content-Length, connection-close and bounded chunked transfer framing.
Redirects, HTTP content compression, proxies, IPv6 and private-repository
authentication are outside this example's scope.

TLS is [embedded-tls 0.19](https://docs.rs/embedded-tls/0.19.0/embedded_tls/)
with default features disabled and software RustCrypto AES/P-256/SHA-256.
It supports TLS 1.3 AES-128-GCM/SHA-256, sends SNI and requests HTTP/1.1 through
ALPN. This example uses its `UnsecureProvider`: it performs the handshake and
encrypts traffic without validating certificates, hostnames or server
signatures. Git pack checksums, object IDs and tree structure are still
validated. TLS state and record buffers live in kernel-owned memory and are
wiped on close.

Smart mounts support SHA-1 packs with up to 32,768 objects and delta depth 64.
The snapshot must fit the cache budget. Ordinary HTTP still requires loose
objects at its origin. SHA-256 Git, persistent disk caching and background
refresh remain extensions.

Files, executables, symlinks, nested/empty trees and byte-string filenames are
supported. Invalid VFS components and names over 255 bytes are rejected.
Gitlinks appear as empty directories; submodule repositories are not fetched.
Ownership is root, timestamps are zero, and directory link counts are two.

## Tests

```sh
cargo test -p git-zero-kernel-codec --locked
make -C examples/linux-kernel
python3 examples/linux-kernel/tests/smoke.py
# Also verify a live public GitHub checkout from the guest kernel:
python3 examples/linux-kernel/tests/smoke.py --github
# For another matching kernel:
python3 examples/linux-kernel/tests/smoke.py --kernel /path/to/bzImage
```

The smoke test creates objects with real Git, generates an initramfs and loads
the module in an **unprivileged QEMU VM** using software emulation. It does not
load the module into the host. It requires QEMU, Python 3, Git, a C compiler,
`ldd`, OpenSSL, the matching `e1000` module and a bootable image matching the module.
The default test only talks to local origins. `--github` additionally requires
internet access from QEMU's user-mode network.

Coverage includes HEAD/commit mounts, request counts, concurrent blob deduplication,
100 missing-name probes, failure TTL/retry, directory iteration, symlinks,
executable modes, pinned commits, offline warm reads, LRU eviction/refetch,
rejected writes/remounts, malformed HTTP/objects, failed-mount cleanup,
unmount and unload. HTTPS coverage includes a real Git smart backend, fragmented
chunks, handshake-only TLS with a self-signed certificate for a different
hostname, shallow packs and deltas. `--github` mounts
`octocat/Hello-World` using kernel DNS and verifies its README bytes.
The userspace origins are only test fixtures. Logs go to
`build/qemu.log`.
