# Finding your way from Git to git-zero

Start with the Git operation you know. The crate split separates shared object
and wire primitives (`git-zero-core`), client negotiation (`git-zero-client`),
server negotiation (`git-zero-server`), and native I/O (`git-zero-rustix`). The
root `src/lib.rs` is an embedding facade; it reexports server APIs as
`git_zero::http` and client APIs as `git_zero::client`.

This map compares the source with [Git v2.53.0](https://github.com/git/git/tree/v2.53.0),
our pinned interoperability reference. It maps responsibilities, not complete
feature parity or a line-for-line port.

| In upstream Git | Start here in git-zero |
| --- | --- |
| `http-backend.c`: service dispatch, raw refs and loose objects | [server/http_backend.rs](../crates/git-zero-server/src/http_backend.rs), with URL/media-type validation in [server/route.rs](../crates/git-zero-server/src/route.rs) |
| `upload-pack.c`: wants, haves, history selection and pack delivery | [server/upload_pack/mod.rs](../crates/git-zero-server/src/upload_pack/mod.rs); request parsing, ACKs and sideband output in [negotiation.rs](../crates/git-zero-server/src/upload_pack/negotiation.rs), shallow/filter arguments in [extensions.rs](../crates/git-zero-server/src/upload_pack/extensions.rs) |
| `upload-pack.c`: advertised refs and capabilities | [server/upload_pack/advertisement.rs](../crates/git-zero-server/src/upload_pack/advertisement.rs); host want permissions in [policy.rs](../crates/git-zero-server/src/upload_pack/policy.rs) |
| `serve.c`, `ls-refs.c`: protocol v2 commands and ref queries | [server/v2.rs](../crates/git-zero-server/src/v2.rs) |
| `builtin/receive-pack.c`: push commands, capabilities and status | [server/receive/](../crates/git-zero-server/src/receive/mod.rs); the host supplies quarantine storage, ref transactions and publication |
| `fetch-pack.c`, `connect.c`: client discovery and negotiation | [client/smart.rs](../crates/git-zero-client/src/smart.rs), with transport calls in [client/lib.rs](../crates/git-zero-client/src/lib.rs) |
| `transport.c`, `remote-curl.c`, `http.c`: transport and HTTP I/O | `Transport` in [client/lib.rs](../crates/git-zero-client/src/lib.rs); native framing/sockets in [rustix/network/](../crates/git-zero-rustix/src/network.rs) |
| `pkt-line.c` | [core/pktline.rs](../crates/git-zero-core/src/pktline.rs) |
| `refs.c`, `refs/`: ref names, symbolic resolution and HEAD | [core/repository.rs](../crates/git-zero-core/src/repository.rs); HTTP client ref lookup in [client/reference.rs](../crates/git-zero-client/src/reference.rs) |
| `object.c`, `object-file.c`, `commit.c`, `tree.c`, `tag.c`: object formats and parsing | [core/object.rs](../crates/git-zero-core/src/object.rs), [object/parse.rs](../crates/git-zero-core/src/object/parse.rs), [object/links.rs](../crates/git-zero-core/src/object/links.rs) |
| `object-file.c`: loose-object I/O | Loose writers in [core/pack/encode.rs](../crates/git-zero-core/src/pack/encode.rs), pull-based output in [pack/stream.rs](../crates/git-zero-core/src/pack/stream.rs), client fetch/decode in [client/lib.rs](../crates/git-zero-client/src/lib.rs) |
| `packfile.c`, `pack-write.c`, `builtin/index-pack.c` | [core/pack/](../crates/git-zero-core/src/pack.rs): decoding, deltas, encoding, indexes and compressed-entry reuse |
| `fsck.c`, `list-objects.c`, `commit-reach.c`, `shallow.c`: validation and reachable-object selection | [core/validation.rs](../crates/git-zero-core/src/validation.rs), [validation/revision.rs](../crates/git-zero-core/src/validation/revision.rs); shared history/filter rules in [core/fetch.rs](../crates/git-zero-core/src/fetch.rs) |
| `t/`: interoperability and regression tests | [tests/](../tests/README.md), native adapter tests and Worker tests; [upstream harness](../tests/upstream_harness.py) runs selected unchanged upstream tests |

## Follow a fetch

For a buffered request, `http_backend::serve_with_pack` validates the route and
calls `upload_pack::write_upload` for a complete HEAD-only pack. The public
`serve_with_selection*` and `serve_with_revisions` entrypoints go through
`upload_pack::prepare_fetch`: parse the request, authorize wants, select the
closure through `ValidatedRepository`, emit control packets, then encode the
pack. [upload_pack/storage.rs](../crates/git-zero-server/src/upload_pack/storage.rs)
holds the caller-owned request buffers.

[stream::ResponseEncoder](../crates/git-zero-server/src/stream.rs) shares the
same selection preparation and pulls pack/loose bytes as the host accepts them.
[stored](../crates/git-zero-server/src/stored.rs) reuses the protocol parsers for
an external catalog: the host authorizes IDs and selects objects separately.
The [repository host](../examples/repository-host/README.md) connects that API
to durable storage and receive-pack publication.

The public `git_zero_server::serve*`, `stored`, `stream` and `receive` paths
remain the embedding entrypoints. `http_backend` and `upload_pack` are internal
source boundaries.

## Names that need context

- `git_zero::FetchOptions` (from core) controls server-side history and filter
  selection. `git_zero::client::FetchOptions` is a separate client wire-options
  type with `shallow_depth`, `ofs_delta` and `no_progress`. Qualify or alias these
  imports when using both; they are not interchangeable.
- Core `validation` includes the fetch graph walk as well as object/link
  validation. Look there for `select_bounded_fetch` and `select_revision_fetch`.
- Core `pack/receive.rs` decodes incoming pack bytes. Server `receive/` handles
  receive-pack control and status; storage publication belongs to the host.
- The core `Repository` is an immutable object/ref provider, not Git's mutable
  on-disk `struct repository`. Objects, refs, indexes and scratch buffers belong
  to callers. Rust library operations use neither `std` nor `alloc`.

Keep protocol selection and framing separate from object encoding and host I/O.
A new server negotiation rule belongs with `upload_pack` or `receive`; shared
object/graph rules belong in core. Run the [validation checks](validation.md),
including the isolated feature matrix, after moving code across these boundaries.
