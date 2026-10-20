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

The repository host uses its own codec by default at
`examples/repository-host/cloudflare/wrangler.toml`. Its Rust Wasm
module has fixed memory and no allocator/imports. Runtime glue streams incoming
packs into 8 MiB authenticated R2 chunks and records private object metadata in
SQLite. Rust resolves delta ranges, hashes canonical objects and scans typed
graph links before a SQLite transaction publishes refs. The catalog stays in
place when published; refs and stage visibility change atomically. No Git
executable or external codec service participates in this path.

Chunk ownership is recorded before R2 writes. Failed intake reclaims its claims
and deletes bytes only when no other stage owns the same chunk. Deletion keeps
its retry record until R2 confirms it; alarms and cold-start recovery finish
interrupted cleanup. Test-retained quarantines can expire through the private
test API; production jobs never retain an unpublished verified pack indefinitely.

From that Cloudflare directory, `npm ci` and `npm run test:streamed` build the
Wasm module and run actual Wrangler R2/SQLite tests. Tests include ordinary Git
push clients in SHA-1/SHA-256, thin incremental packs, protocol preference 0/1/2,
atomic updates, push options, deletion, HTTP chunking, concurrent stale updates,
invalid graphs and cold restarts. The config defaults to `TEST_MODE=false`;
private fixture endpoints require both test mode and authentication. Public
receive requests use `/<repository>.git/info/refs?service=git-receive-pack` and
`/<repository>.git/git-receive-pack`, with `HOST_TOKEN` supplied as a secret.

The own-codec path also serves full clone/fetch in protocols 0/1/2, including
SHA-256, raw ref prefixes, symbolic HEAD, annotated-tag peeling, gzip control
bodies and incremental negotiation. The existing Rust request parsers and ACK
encoder serve both repository adapters and the external catalog. A fixed 4 MiB
control arena keeps validated packet storage immutable while iterating wants and
haves. Total Wasm linear memory is bounded at 6 MiB; object bodies remain outside
that arena and the runtime caches remain bounded at 48 MiB.
Each byte cache also has an independent 8,192-entry cap, so empty or tiny
representations cannot exhaust memory through map metadata.

Fetch selection uses SQLite queues, not repository-sized JS arrays. Compressed
entries are copied in physical pack order; OFS deltas become REF deltas. When a
base is outside the selected pack, Rust reconstructs and streams a canonical
stored-block zlib representation instead. This keeps a full pack independent of
the client's existing bases. Real Git clones, fetches, checkouts and strict fsck
pass through Wrangler for both formats and all three protocol versions.

For a cold full fetch, publication records which packs contain only new,
distinct object IDs. If these disjoint packs cover the complete published
catalog, fetch copies their complete entry regions instead of selecting every
object. Rust writes the combined count and checksum; shifting an entire region
preserves its internal OFS distances, and catalog coverage proves that every
REF base is included. A single covering pack streams unchanged. This path is
available only without known haves or shallow/filter extensions. It can include
extra published objects from the same repository: `HOST_TOKEN` grants access to
that entire repository. Otherwise fetch uses the per-object selection above.

The new storage path does not yet enable shallow/filtered fetch. It has not
passed a full Linux push through Wrangler. Linux-scale metadata storage,
ingress beyond Cloudflare request limits and measured hosting cost remain open;
component pack-decoder measurements are not end-to-end hosting results. The
former native Git gateway and its local service-registration API are retired.
Verification in this work is local only; no live Cloudflare deployment is
requested or treated as a completion gate.

The same modules also run directly under the pinned workerd binary using
`examples/repository-host/cloudflare/workerd.capnp`. This deployment candidate
uses persistent SQLite on the host and signed R2 S3 requests for opaque chunks;
it does not run a Git executable. The same real-Git round-trip suite passes
against direct workerd, with an independent S3 signature verifier and byte store.
Wrangler tests continue to exercise the native R2/SQLite bindings separately.

Build with `npm ci && npm run build:streamed`, then set `HOST_TOKEN`,
`R2_S3_URL` (the full bucket URL), `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, and
an absolute `GIT_ZERO_STATE_DIR`. `npm run serve:standalone` starts workerd on
loopback, with test routes disabled. A systemd unit and streaming TLS proxy
example are provided in that directory. The state directory, namespace key in
the config and R2 bucket together identify the durable repository; preserve them
across restarts and backups. workerd's disk-backed Durable Object API is
experimental, so this candidate pins the runtime through the lockfile and still
requires Linux-scale and live-environment validation before shipping.

Plain blobs need no second reconstruction after the intake decoder has hashed
their complete bodies; they have no structural links. Small authenticated blob
bodies also warm the bounded base cache directly. Tree-name uniqueness rows
are scratch for one tree and are removed only after its full scanner and
canonical digest succeed; typed graph edges remain available to fetch planning.

For a reviewable scale run, use `node scripts/benchmark.mjs --source REPO
--work-dir NEW_DIRECTORY` from the Cloudflare directory. The default runtime
freezes the current bundle and starts actual Wrangler R2/SQLite, so later edits
cannot change a running measurement. Ordinary Git pushes a complete source,
clones it, pushes one new commit without changing the source branch, restarts
the host and fetches the increment. Strict fsck diagnostics must match the
source, preserving historical badFilemode warnings. The report records phase
times, application SQL/R2 counters, database size and local server process-tree
CPU/RSS samples. These process samples include runtime/development services;
they are not Cloudflare billing CPU. `--runtime workerd` measures direct workerd
with the independently signed local S3 byte fixture instead.
The diagnostic comparison streams the selected source closure, retaining only
the diagnostic IDs; unreachable source tags/objects do not create false clone
failures. `--baseline-report REPORT` can reuse a successful native source fsck
from the same repository, revision and object format. Server push/clone/fetch
still run against fresh storage, and the source visibility walk runs again.

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
