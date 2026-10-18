# Current architecture and capabilities

## What works

- Borrowed entries become nested trees, regular/executable files, symlinks,
  and one deterministic commit with **no parents**. Empty trees are supported.
  `HEAD` defaults to `refs/heads/main`; callers can configure branch/tag aliases.
- Incremental root-entry construction in caller-owned slots, with configurable
  author/committer identities, Unix times, timezones, and exact commit messages.
- Read-only smart HTTP discovery and `git-upload-pack` over protocol v0/v1/v2,
  including stateless v2 `ls-refs` and `fetch` with bounded selected packs.
- Ordinary Git HTTP, usually called **dumb HTTP**: `HEAD`, `info/refs`, branch
  refs, and zlib loose objects. No filesystem repository is needed.
- SHA-1 and SHA-256 repositories with format-specific object IDs, refs and packs.
  Smart HTTP advertises the repository's object-format capability.
- Complete packs generated on demand from unpacked objects. The encoder
  streams valid zlib/DEFLATE stored blocks, so it uses bounded memory but does
  not yet compress data or create deltas. Identical blobs and subtrees are deduplicated.
- Optional caller-owned object metadata and ID indexes for constant-time
  enumeration and binary-search lookup without repeated scans or hashing.
- Bounded readers and resumable hashing for large/generated objects, with
  caller-sized scratch for pack and loose-object responses.
- Pull-based pack/HTTP response cursors with bounded inline state, backpressure,
  cancellation and caller-owned request intake. Native close-delimited responses
  and the Worker start/read/cancel ABI stream without response-sized buffers.
- Opt-in validation and indexing of custom repositories, checking object IDs,
  tree order and links, commit parents, tag targets, and exact reachability from
  HEAD and independent refs. Symbolic ref chains and final peeled tags are supported.
- Bounded multiple-want selection and packing of just the requested closures,
  common-history exclusion, absolute/relative deepening and unshallowing, plus
  `blob:none` and `blob:limit` filters, using caller-owned traversal arrays,
  scratch and a graph-read byte limit.
- A transport-independent client that discovers refs, resolves `HEAD`, downloads
  and checksum-validates smart packs, fetches several tips with optional shallow
  negotiation, and fetches dumb loose objects.
- Incremental zlib inflation with caller-owned history/output buffers, stored/
  fixed/dynamic DEFLATE support, Adler-32 validation and output/expansion limits.
- Loose-object envelope validation before body exposure, plus a pack entry
  reader with wire CRCs, canonical object IDs and bounded offset/reference deltas.
- Complete pack materialization into caller-owned bodies and a sorted ID index,
  plus borrowed commit messages, tree entries and annotated-tag targets.
- Streaming pack and Git v2 index generation with caller-owned metadata, in both
  object formats. Publishing packed dumb HTTP repositories remains planned.
- Optional complete-HEAD and selected-pack caches, with versioned closure/format/
  encoder keys, integrity checks, a caller-buffer cache, and a native disk backend
  with atomic publication and cooperative per-key producer leases.
- IPv4/IPv6 server listeners, an IPv4 client adapter and a Cloudflare Workers Wasm demo.
- Native file-backed raw objects and verified prepacked responses, with positional
  reads and caller-owned scratch, using `rustix` without `std` or `alloc`.
- Worker immutable inline/KV/R2 metadata snapshots, explicit public binding/header
  selection, optional tree layout, and bounded canonical repository import.
- Parallel interoperability tests mixing real Git and git-zero clients/servers.

The client discovers and selects refs, downloads checksum-validated v0 smart
packs or dumb loose objects, and supports shallow/multiple-tip requests. Shared
core APIs decode objects, materialize complete non-thin packs and traverse
checkout trees; the kernel example uses them to expose cached snapshots and refs.
Complete pack materialization currently supports SHA-1; the entry reader also
supports SHA-256 packs.
A complete native clone/checkout writer, dumb repository walker, client filters,
client protocol v1/v2, push and standalone `git://` transport remain planned.

Server partial clones can perform ordinary lazy checkout when the host explicitly
grants retrieval from its immutable visible closure. The default advertised-ID
want policy still requires `--no-checkout`; the grant never exposes objects
outside the supplied validated repository view.
Time/exclusion deepening and tree/sparse/combine filters remain unsupported.
The native server uses blocking sockets and HTTP/1.0 close-delimited responses.
Hosts can set inclusive complete-wire/decoded-body quotas and independent
accepted-socket read/write timeouts; default quotas remain unset and timeouts
remain ten seconds. TLS, chunked intake, keep-alive, request deadlines and
poll-based concurrency remain host work. See the
[native admission contract](../crates/git-zero-rustix/README.md).
The Worker has streamed responses and
async snapshot persistence, but no complete-pack KV/R2 cache integration or
deployed performance measurements. Server parity is still incomplete; further
client and receive-pack work follows its completion gate.

## Workspace

| Location | Responsibility |
| --- | --- |
| `src/` | `git-zero` facade with optional client/server reexports |
| `crates/git-zero-core/` | Objects, repositories, packet lines; `read` and `write` features |
| `crates/git-zero-client/` | Transport-independent HTTP discovery and fetching |
| `crates/git-zero-server/` | Smart/dumb HTTP repository routing |
| `crates/git-zero-rustix/` | `no_std` file/socket adapters; small `std` process wrapper |
| `examples/cloudflare-worker/wasm/` | `no_std` raw Wasm ABI with fixed buffers |
| `examples/cloudflare-worker/src/` | JavaScript host for Cloudflare metadata and HTTP |
| `examples/linux-kernel/` | Kernel VFS/socket adapter and `no_std` Rust codec for mounting Git snapshots |
| `tests/` | Core checks, real Git pack validation, parallel HTTP matrix |

The core and native adapter libraries never enable `std` or `alloc`. The native
CLI uses `std` for startup arguments, stdout, and diagnostics. JavaScript and the
test harnesses use their host runtimes' memory management; the no-allocation
guarantee applies to Rust core/adapter operations, not the surrounding platform.
