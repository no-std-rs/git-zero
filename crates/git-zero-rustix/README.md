# git-zero-rustix

Allocation-free `no_std` rustix adapters for git-zero. The host owns buffers,
object manifests and file descriptors.

- `client`: blocking IPv4 HTTP/1.0 transport.
- `server`: IPv4/IPv6 listeners, request parsing and response framing.
- `storage`: immutable raw file objects and verified prepacked responses.
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

`listen(port)` retains IPv4 loopback binding. `listen_on(SocketAddr)` binds an
explicit numeric IPv4 or IPv6 address, and `local_address` reports the assigned
address when port zero is used. IPv6 sockets set `IPV6_V6ONLY`: hosts that need
both families create separate listeners. Accepted sockets retain close-on-exec
and the existing ten-second send/receive timeouts. The CLI accepts a port or a
numeric address, for example `git-zero serve '[::1]:8080'`, and prints the bound
URL. Its existing client transport still accepts IPv4 addresses only.
