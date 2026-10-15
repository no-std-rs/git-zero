# git-zero-rustix

Allocation-free `no_std` rustix adapters for git-zero. The host owns buffers,
object manifests and file descriptors.

- `client`: blocking IPv4 HTTP/1.0 transport.
- `server`: loopback listener, request parsing and response framing.
- `storage`: immutable raw file objects, verified prepacked responses and a
  selected-pack disk cache.
- `cli`: the `git-zero` demonstration binary; enables all adapters and validation.

All features are enabled by default. Disable defaults to select adapters.
The library uses no `std` or `alloc`; the CLI uses `std` for process startup.

See [git-zero](https://github.com/no-std-rs/git-zero) for usage and limitations.

The server reads `Git-Protocol` and `Content-Type` into shared request metadata.
Gzip uploads are decoded into the request storage after the wire bytes; headers,
compressed body and expanded body must all fit. Inflation borrows a fixed 32 KiB
stack history window and validates the gzip checksum and size before routing.
Duplicate framing/selector headers, truncated bodies and unsupported transfer or
content encodings are rejected. Chunked framing remains unsupported.

`respond_stream` sends a prepared `http::stream::ResponseEncoder` with HTTP/1.0
close-delimited framing and caller-owned output staging. It writes all staged
bytes before pulling again, cancels on errors, and requires the host to close the
socket on return. It remains a blocking adapter; readiness polling, keep-alive,
chunked input, TLS and HTTP/1.1 framing remain future work.

With `storage`, `cache::DiskCache` implements the core selected-pack cache backend.
The host retains a trusted directory descriptor, a fixed resident byte buffer
and a producer nonce unique within that directory. Use it through
`git_zero::cache::selected::write` after authorization, visible-ref/reachability
checks and graph selection. Each lookup copies one regular file into resident
storage; core validates its exact canonical object set and checksums before
output. The resident capacity bounds disk reads and accepted pack size. Match
it to the request's pack-byte limit. Cache files use the 48-byte selected key
encoded as 96 hexadecimal characters plus `.pack`; HTTP pack/index publication
uses the actual returned pack checksum separately.

Insertions use exclusively created private staging files, complete writes,
file synchronization and a final atomic same-directory rename. Failed insertion
preserves published entries. Concurrent producers may repeat work; readers see
one complete publication and retain independent resident bytes across replacement
or eviction. A sink failure can leave a complete entry for a later retry.
Disabled mode bypasses reads/inserts; explicit key eviction and saturating
hit/miss/insertion/eviction counters expose backend activity. Hosts supply
authorization namespace directories, total disk quotas, cold-miss coordination,
stale staging-file cleanup after crashes and directory durability policy.
