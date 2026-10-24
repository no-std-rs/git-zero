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
content encodings are rejected. Chunked intake requires the explicit APIs
described in [Native deadlines and chunked intake](#native-deadlines-and-chunked-intake).
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
socket on return. It remains a blocking adapter; keep-alive, TLS and HTTP/1.1
response framing remain future work. Opt-in chunked request intake and
deadline-driven readiness polling are described below.

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
scheduling and cancellation. The allocation-guarded `admission` socket tests cover
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

The `selected-cache` example integrates the selected HTTP hook with either
caller-owned `FixedCache` storage or `DiskCache`. Export a canonical fixture
using `tests/server_fixture.py`, then run:

```sh
cargo run -p git-zero-rustix --example selected-cache -- --fixture /tmp/fixture
```

It prints an ephemeral loopback clone URL. Add `--cache-directory /tmp/private-cache`
to use an existing private directory, `--port PORT` to choose a port, and
`--pack-bytes BYTES` to set both raw-pack staging and resident cache capacity
(default 1 MiB, maximum 16 MiB). It separately reserves 64 KiB for a request,
raw-pack capacity plus 128 KiB for the framed response, 256 object/ID slots,
64 wants, 512-byte read/parse scratch and a 16 MiB graph-read budget. Fixture
admission retains its 256-object/8-MiB raw-body bounds. Exceeding any budget
fails instead of truncating or falling back.

The example is a single-producer synchronous host. It buffers the entire body
before emitting HTTP success; cache failures replace provisional protocol bytes
with one error response. It performs request authorization/selection on every
hit, and never caches refs, ACKs, shallow updates or sideband framing. Hosts own
authentication, namespace selection, process lifetime and disk quotas. Optional
producer leases remain a separate host policy. The real-Git test matrix covers
memory/disk miss and hit, exact selected closures and true incremental fetch
in both formats and v0/v1/v2; reopening the disk backend across A -> B -> A
proves retained content reuse. No Worker or async pack storage is implied.

The cache example stays strict unless `--allow-reachable-wants` explicitly
grants the entire supplied validated visible closure, with a 64-occurrence want
cap. Its buffered responder receives `WantOptions` on every call; revoking that
grant denies unadvertised wants before consulting even a warmed cache. This
trusted host flag never comes from HTTP input and does not replace authentication
or an authorized immutable repository projection.

Header-specific admission uses `read_request_with_policy` and `RequestPolicy`.
`HeaderLimits` independently bounds complete header bytes (request line and
final delimiter included), field count, name bytes and raw value bytes after the
colon. Leading/trailing whitespace consumes the value budget. Unknown and
repeated ordinary fields consume quotas too. Defaults are unset and preserve
`read_request` / `read_request_with_limits` behavior. An incomplete header stops
at the byte cap without consuming the first excess byte. Each field's syntax is
validated before its field quotas; accepted fields still follow the existing
framing/duplicate rules. Exhaustion returns `LimitExceeded` before body intake,
gzip expansion or repository preparation. Close failed requests and reuse their
storage only after dropping all provisional views.

The CLI accepts these optional inclusive limits, including zero:

```sh
git-zero serve 8080 --max-header-bytes=4096 --max-header-fields=32 \
  --max-header-name-bytes=64 --max-header-value-bytes=1024 \
  --max-wire-bytes=65536 --max-body-bytes=32768
```

Values are decimal byte/count budgets. Duplicate/unknown flags, signs and
out-of-range values fail before listening. The address, when present, comes
first. These controls leave accepted-socket per-I/O timeouts unchanged and do not
establish aggregate request/idle deadlines.

Packed dumb publication is described in [the publication guide](../../docs/packed-dumb.md).
`respond_published` streams generated packs/indexes;
`FilePublication::from_fds` verifies pinned complete files and
`respond_file_published` streams them with independent offsets. Both cancel on
socket/provider failure. The CLI's immutable demonstration snapshot now serves
its generated pack/index alongside loose objects.

## Native deadlines and chunked intake

`read_request_with_deadlines` adds `RequestDeadlines { total, idle }` to a request
policy. `total` starts when intake begins and never resets on progress. `idle`
resets only after a positive read. `respond_stream_with_deadlines` and
`respond_published_with_deadlines` apply independent `ResponseDeadlines` to
streamed/known-length output; successful writes reset idle, while total remains
absolute. Unset defaults preserve the original blocking APIs and their overhead.

The CLI accepts positive, unique `--request-deadline-ms=N`, `--idle-deadline-ms=N`,
`--response-deadline-ms=N` and `--response-idle-deadline-ms=N`. Deadline exhaustion
returns `LimitExceeded` (the CLI's existing 413 admission response before output).
A shorter accepted-socket timeout retains `Backend`. Poll plus per-call
nonblocking I/O preserves descriptor flags and read/write timeout settings.

Intake covers HTTP bytes plus the final parse/inflation check. Response checks
bracket provider pulls and EOF, cancelling on expiry without a successful protocol
terminator. These calls cannot preempt synchronous provider code. Accept, Git
preparation between phases, and total connection lifetime require host policy;
the example still serves accepted connections serially.

`--chunked-requests` explicitly enables bounded HTTP/1.1 transfer decoding.
Library hosts can call `read_request_chunked` with `ChunkedLimits`: defaults allow
1024 nonzero chunks and 32 wire bytes per size line. Complete encoded wire remains
resident, while chunk bodies compact in place. Wire quotas count chunk framing;
body quotas count transfer-decoded identity bytes or subsequently inflated gzip
bytes. Gzip expansion uses storage beyond the complete encoded wire.

Duplicate transfer fields and Content-Length/Transfer-Encoding conflicts fail.
Unsupported transfer codings, extensions and nonempty trailers are rejected
explicitly. HTTP/1.0 chunked requests fail. Retained bytes after a final chunk fail;
there is no keep-alive or pipeline processing, and every response closes the
connection. Existing entrypoints continue to reject Transfer-Encoding by default.

`read_request_from` and `read_request_from_chunked` expose the same parser to a
host-supplied blocking read callback, including TLS streams. The callback owns
peer/certificate validation, TLS records, buffering, deadlines and cancellation;
it fills at most the supplied slice and returns zero only at EOF. Parser errors
invalidate storage and require closing that transport. This is a documented
transport boundary; no TLS provider or authenticated TLS test is bundled here.
