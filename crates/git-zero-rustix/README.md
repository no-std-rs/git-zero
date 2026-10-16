# git-zero-rustix

Allocation-free `no_std` rustix adapters for git-zero. The host owns buffers,
object manifests and file descriptors.

- `client`: blocking IPv4 HTTP/1.0 transport.
- `server`: IPv4/IPv6 listeners, request parsing and response framing.
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
Header delimiter scanning retains three overlap bytes between reads, so each
possible four-byte delimiter position is examined at most once. Header and body
bytes still share the caller's fixed request storage; fragmentation does not
raise that limit. Metadata uses one UTF-8 check and one field pass, with HTTP
token names, control-free values and space/tab padding. Request lines reject
embedded CR/LF. The `http_framing` socket tests cover binary bodies, malformed
requests, exact capacity and recovery with reused storage. Scan unit tests prove
the work bound for one-byte fragments through a 64 KiB header.

`read_request_with_limits` adds inclusive host quotas through `RequestLimits`:
`max_wire_bytes` bounds the request line, headers, delimiter and encoded body;
`max_body_bytes` bounds the decoded body, including identity uploads. `None`
leaves a policy unset; caller storage always bounds memory. An explicit policy
failure returns `LimitExceeded`, physical capacity returns `BufferTooSmall`,
and a tied exhaustion boundary reports the policy failure. Exact limits allow
empty bodies and valid empty gzip members. Header validation precedes known
body-length policy checks. Gzip retains the full physical expansion suffix,
the existing ratio/slack settings and checksum validation. A malformed stream
can fail before a size limit is reached. Every failure invalidates provisional
storage: close the connection and discard it before repository preparation.
`read_request` retains unset policies and its previous behavior.

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

`listen(port)` retains IPv4 loopback binding. `listen_on(SocketAddr)` binds an
explicit numeric IPv4 or IPv6 address, and `local_address` reports the assigned
address when port zero is used. IPv6 sockets set `IPV6_V6ONLY`: hosts that need
both families create separate listeners. Accepted sockets retain close-on-exec
and the existing ten-second send/receive timeouts. The CLI accepts a port or a
numeric address, for example `git-zero serve '[::1]:8080'`, and prints the bound
URL. Its existing client transport still accepts IPv4 addresses only.

`accept_with_timeouts` takes explicit `AcceptedSocketTimeouts`; its default has
both read and write set to ten seconds, matching `accept`. `None` disables the
corresponding child setting, and zero durations are rejected before accepting.
The listener is unchanged, and configuration failure closes the accepted
descriptor. These are per-blocking-call socket timeouts, with OS rounding;
partial operations and retries can extend a whole request or response. They
are not accept or aggregate deadlines. Hosts still own connection admission,
polling and cancellation. The allocation-guarded `admission` socket tests cover
quota precedence, gzip integrity at exact quotas, unread excess bytes, both IP
families, timeout stalls, write backpressure and stream cancellation. The native
`references` Git host uses explicit limits/timeouts and verifies rejection and
recovery before its existing smart/dumb comparisons in both object formats.

On platforms with rustix `flock` support, `DiskCache::try_lease(key)` offers
cooperative cold-miss coordination. Contended locks return `None` without waiting;
disabled caches also return `None`. File operations remain synchronous. All
producers in one namespace use the same lease
protocol: authorize/select, try the lease, recheck the cache after acquisition,
then generate only on a remaining miss. The host controls retries and deadlines;
dropping the guard explicitly unlocks and closes its descriptor on cancellation
or failure. Unrelated keys remain independent. Disabled caches create no lock
files. Producers that skip this optional protocol retain atomic complete
publication but may duplicate work.

Lease files are empty regular files named with the same selected key plus
`.lock`. They persist across pack eviction and producer exits. Hosts reclaim
them only when all users of that namespace are quiescent; replacing a live lock
file would split its identity. Advisory lock scope depends on the filesystem
and mount configuration. Hosts account for lock files in directory quotas and
provide a distributed coordinator when that is required.


The demonstration command `git-zero serve ADDRESS --allow-reachable-wants`
explicitly grants access to the whole validated visible snapshot closure.
The flag is absent by default; unknown flags fail. The example uses a 1,024
want-occurrence cap in addition to its fixed selection arrays, request storage
and graph-read limit. It is a host configuration option, never an HTTP option.
The bounded `upstream-cgi` fixture host accepts the same final flag. Production
hosts must authenticate/select their own immutable projection before constructing
a policy-aware selected encoder; neither socket parsing nor a cache grants access.
