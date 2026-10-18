# Repository hosting: compressed segments, local first

Design dated 2026-10-17. Build and measure the local twin before spending money on
Cloudflare. The intended service accepts ordinary Git clone, fetch and push over
smart HTTP, stores compressed history, and does bounded repacking inline. A
performance advantage over a tuned Git server is a benchmark goal, not an assumed
property.

The primary target is total cloud cost per push/fetch within latency, memory and
CPU limits. A modest compressed-storage premium is acceptable when it reduces
rewrite traffic, storage operations or codec work. Minimum pack size is a
secondary goal. Keep read amplification, retained bytes and maintenance debt
bounded; the local storage premium is a measurement, not a production allowance.
Do not add full-history repacks just to match Git's disk footprint. Require
workload-based cost evidence before choosing that tradeoff.

The [hosting prototype](../examples/repository-host) and local loopback benchmark
use Rust. The initial native codec delegates pack validation and encoding to
Git's `index-pack` and `pack-objects`. Its shared storage and control logic does
not make those native subprocesses available inside a Worker. Replacing that
adapter with bounded Rust/Wasm pack processing is a separate required step.
The existing allocator-free Rust core remains distinct from the hosting prototype.
Linux-scale streaming, paged indexes, durable reader leases, production
authorization and Cloudflare execution remain design work until specifically
implemented and tested.

## Current local prototype

- A Rust loopback smart HTTP host uses native Git to validate quarantined pushes,
  with the pinned published native view supplying existing delta bases and graph
  objects. Incoming self-contained compressed packs and Git v2 indexes are split
  into SHA-256-addressed blobs before manifest compare-and-swap. The local native
  adapter uses 16 MiB chunks to share typical packs without a second disk copy;
  the shared engine defaults to 8 MiB. Shared Rust
  code owns chunk integrity and manifest publication; native codec orchestration
  remains platform-specific.
- The common HTTP path invokes Git's stateless RPC directly. Rust prepares the
  disposable bare layout and reads/writes native ref snapshots; the durable
  manifest remains authoritative. Encoded requests retain the reference HTTP
  backend's decoding and validation path.
- The native `hosting` Cargo profile optimizes for throughput. The default
  `release` profile retains its size optimization for the portable core.
  Large incoming packs are hashed alongside strict validation. Checksums and
  input buffers are reused only after every byte matches the immutable codec
  output; completed thin packs and compaction outputs use ordinary hashing.
- Restart rebuilds the native Git view from exactly the authoritative manifest
  and verified chunks, discarding unpublished native packs before later push
  validation. Git's disposable codec views use `core.fsync=none`; authoritative
  blob and manifest writes sync files and directories before acknowledgment.
  Recovery first stabilizes a visible manifest after an ambiguous publication
  error, before it may reclaim older chunks. The loopback runner serves
  requests serially with one writer.
- Snapshot batch reads return an opaque, owned `VerifiedChunks` proof. Its safe
  constructor checks every ordered payload's declared length and SHA-256 key;
  consuming it requires exact requested keys, lengths and order. The HTTP bridge
  validates complete framing and each batch before later requests, then forwards
  proofs through both storage wrappers without another hash pass. Default trait
  methods verify untrusted/local results using their existing batch getter.
  Every chunk is verified before the first native pack/index is materialized,
  including repeated keys and oversized legacy scalar reads. This changes native
  CPU work only: read operations, bytes, limits and cache behavior stay fixed.
  The observed 27.8 ms cold preparation increase motivated this bounded change
  but does not establish causation or recovered latency. Warm tails with zero R2
  reads are unrelated; known cloud dollars and receipts are unchanged.
- A push can run one inline compaction job selecting the four smallest packs at
  a level when their combined input is at most 8 MiB. Rust validates pack/index
  checksums, entry CRCs and delta dependencies, copies compressed payloads without
  inflation, converts offset deltas to ID-based references and writes a v2 index.
  It preserves source physical order and deduplicates IDs. Strict native Git
  validation still runs before new untrusted objects can enter this path.
- Publication records retired chunk keys before cleanup. This serial host
  deletes superseded native packs and chunks after durable publication; failures
  remain retryable and block further writes until cleanup succeeds. Recovery
  also removes unpublished chunks. Prepared responses own their bytes, leaving
  no old-generation storage reader when the next request runs. Production
  concurrent streaming still requires the leases below. This cleanup removes
  replaced storage, not unreachable objects inside live packs.
- The local Cloudflare bridge negotiates bounded CAS/retirement fusion during
  codec registration. At most 256 debt keys can share one callback; each actual
  R2 delete and catalog operation remains charged. The DO syncs publication and
  finishes CAS success accounting before cleanup, then syncs catalog changes
  before returning ordered per-key outcomes. Durable debt stays complete;
  ambiguous replies force authoritative Native recovery before another request.
  Default/local and older adapters retain CAS followed by scalar cleanup. See
  the [service contract](../examples/repository-host/cloudflare/README.md).
- Downloads reuse existing compressed streams with `pack.window=0`. Requests
  containing framed client `have` lines use a one-entry delta search instead,
  producing useful thin deltas without a full-history delta search.
- Single-chunk native files share immutable blob inodes, including after restart.
  Larger logical packs spanning chunks still need a materialized native view.
  This optimization belongs to the filesystem adapter; the intended Worker
  reads ranges from R2 instead.
- Responses and advertisements use generation-keyed caches incorporating method,
  target, Git protocol, content type, content encoding and request body. Fields
  are length-prefixed and hashed into fixed-size keys. Shared immutable buffers
  avoid copying a whole pack on hits. The cache charges bodies, keys and
  header/entry overhead against 32 MiB, limits entries to 256 and caps each body
  at 16 MiB. Empty responses are not cached; writes clear the cache.
  Production authentication and
  cache policy isolation described below remain unimplemented.
- Request bodies are limited to 64 MiB and live compressed packs plus indexes
  to 128 MiB. Ref snapshots are limited to 16,384 refs and 4 MiB of names/IDs;
  ref-only pushes cannot grow manifests and advertisements indefinitely.
  Recovery removes abandoned manifest-write temporaries as well as orphan chunks.
  Whole-pack buffers and native Git processing do not establish a
  Worker-safe memory or decompression bound. Paginated indexes, range reads,
  resumable streaming, production maintenance continuation and large imports
  remain work. Local tests include sustained pushes and varied want/have fetches,
  asserting against read-triggered storage writes and unbounded caches.

The R2/Durable Object wrappers have mock tests and a runnable
[local Wrangler/workerd adapter](../examples/repository-host/cloudflare/README.md).
Its Worker gateway forwards Git HTTP to the Rust native codec, which uses actual
locally emulated R2 and Durable Object SQLite services. Object inventory,
per-binding operation counters and dated cost scenarios are exposed as JSON.
Cold mode fully reloads the native view through counted R2 reads; warm mode
explicitly retains that filesystem cache. This is a local hybrid, without a
deployable Worker Git codec. The sections below describe the scalable service
beyond this prototype.

## One engine, interchangeable services

Keep Git negotiation, validation policy, selection and compaction outside
Cloudflare bindings. Express platform capabilities as Rust traits and pass their
implementations into the shared engine. Keep the pack codec behind its own
interface: native Git is useful for local verification but is not a Worker codec.

| Service contract | Local twin | Cloudflare adapter |
| --- | --- | --- |
| Immutable blob put, head, range read, delete | Files under a repository root | Private R2 bucket |
| Read repository generation; atomically compare and publish state | Serialized state update and atomic rename | Repository Durable Object and transactional storage |
| Disposable cache with byte/TTL budgets | Bounded in-process cache | Workers Cache API and bounded isolate memory |
| Clock, cancellation and work budgets | Injected clock and abort signal | Same interface around request lifetime |
| Durable maintenance cursor and wakeup | Persisted state and local scheduler | Durable Object storage and alarms |
| Pack decode, verification and encode | Native Git codec initially; Rust codec later | Bounded Rust/Wasm codec required |

The table is the target contract, including capabilities beyond the initial
prototype. The implemented `BlobStore` and `StateStore` traits share immutable
chunk storage and manifest publication logic. `LocalServices` persists blobs and
JSON manifests with file sync, atomic rename and directory sync. One instance
and its clones support concurrent requests; independent instances or processes
must not write the same root. The synchronous `R2BlobStore` and
`DurableStateStore` wrappers delegate to a `CloudflareBindings` trait; contract
tests exercise that delegation with a mock and real local workerd bindings over
the HTTP service bridge. Async Rust streaming bindings and a Worker Git codec
remain unimplemented.
Eventually use the
[Workers Rust/Wasm SDK](https://developers.cloudflare.com/workers/languages/rust/)
around R2, Durable Objects and cache bindings. Prefer caller-owned buffers and resumable
operations so portable pack and graph logic can move into `no_std` Rust; native
filesystem/process handling belongs in the local adapter.

A local adapter must state whether it supports one process or multiple
writers. Atomic rename alone does not provide multiprocess compare-and-swap, nor
does it guarantee survival of power loss: durable mode requires file and directory
syncs before acknowledging a push. Match that durability policy when benchmarking
Git. Test service semantics directly, including failed writes and process restarts.

Use one Durable Object per repository as a small control plane for refs,
generations, admission and maintenance leases. Send bulk pack bytes between the
Worker and R2; do not funnel every byte through the coordinator. Keep large indexes
in immutable R2 pages instead of a growing JSON value. Durable Object storage is
transactional and strongly consistent, with a 10 GB paid SQLite object limit and
2 MB row/value limit. These constraints favor a small coordinator.
([Storage API](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/),
[limits](https://developers.cloudflare.com/durable-objects/platform/limits/))

## Physical layout

An R2 object is a storage unit; a Git object is a logical commit, tree, blob or tag.
Do not map these one-to-one. Do not expand compressed packs into permanent raw
objects. Preserve incoming zlib payloads and delta instructions wherever possible.

The scalable layout is an immutable manifest pointing to compressed logical pack
segments and their indexes. Start with an 8 MiB physical chunk target and bounded
prefetch; measure 4, 8 and 16 MiB alternatives. These are tuning hypotheses. Logical
segments may span many chunks. R2 has no append-in-place operation in this design:
an append creates new chunk keys and a new manifest, leaving old chunks intact.

Each manifest records a format version, repository identity, hash algorithm,
generation, refs digest, segment/chunk digests, compressed lengths, index roots
and delta dependencies. Use content-addressed immutable keys under a repository
namespace. Reads of an old generation must never silently resolve through a newer
manifest.

Object indexes map an object ID to type, inflated size, segment, logical byte
offset, compressed length and delta base. A chunk directory translates a logical
range into physical reads. Add sorted, paged object-ID indexes with a small fanout
table, per-segment Bloom filters and immutable index layers; merge a bounded range
of layers instead of rewriting a global index on every push. Cache index pages.
Coalesce adjacent ranges and fetch chunks when many requested entries share one.
R2 supports ranged GETs, but a tiny range is still a storage request.
([R2 Workers API](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/))

Keep delta-compressed representations. When moving entries, either preserve their
relative layout or rewrite offset-delta headers to reference-delta headers while
copying the compressed instruction payload unchanged. Recompute pack headers,
counts, indexes and checksums; concatenating complete pack files does not produce
a valid Git pack. A delta is usable in a response only if its base is included or
the negotiated thin-pack rules permit the client to supply it. The internal
manifest may link older segments, but an exported standalone pack must close over
all bases. Bound chain depth and cross-chunk dependency reads. Recompression and
new delta search are optional measured optimizations, not required on every push.
([Git pack format](https://git-scm.com/docs/gitformat-pack))

Prefer self-contained delta groups within chunks where practical. A single large
object may cross chunk boundaries; stream it with independent limits for the
compressed input, expanded object, delta instructions and dependency traversal.
Do not promise a fixed memory ceiling based only on compressed chunk size.

## Push transaction and durability

1. Authenticate, check repository write permission and reserve bounded admission
   capacity before reading a large body. Parse ref commands, validate ref names,
   and record the advertised old IDs and starting generation.
2. Stream to an unpublished quarantine namespace. Verify pack checksum, exact
   entry framing, canonical object IDs, decompression limits, delta depth and
   resolved base identities. Thin bases must come from the pinned published
   generation or quarantine, never another unfinished push.
3. Validate object structure and connectivity from every proposed new ref through
   commits, trees and tags into quarantine plus the published repository. A pack
   checksum alone does not establish connectivity. Enforce branch policy and
   advertised capabilities; reject unsupported shallow/filter/object-format
   combinations explicitly.
4. Finish and verify immutable chunks and indexes. Persist an immutable candidate
   manifest. Successful R2 writes are visible to subsequent reads; this does not
   create a transaction spanning R2 and Durable Object storage.
5. In one coordinator transaction, compare the expected generation and every old
   ref ID, recheck admission/policy state, then publish the new manifest pointer,
   refs and incremented generation. A stale writer retries/revalidates or fails;
   it must never overwrite a concurrent push. Advertise atomic multi-ref updates
   only when the whole ref command set commits atomically.
6. Report success only after publication is durable. A crash before publication
   leaves collectible orphan chunks. A crash after publication may lose the HTTP
   reply but must leave readable refs and all their required data.

R2 is strongly consistent for completed object operations; use immutable keys to
avoid last-writer-wins replacement. The coordinator is the only publisher. Store
transaction identity and result for ambiguous retries, and sweep expired
quarantine uploads without deleting published dependencies.
([R2 consistency](https://developers.cloudflare.com/r2/reference/consistency/))

## Fetch and cached download packs

Negotiate wants and haves against one pinned generation. Compute the requested
reachable set minus the known common closure, then reuse compressed entries to
stream one valid wire pack. Index and reachability work also need byte, object and
CPU budgets. Reachability bitmaps and commit-graph pages are later acceleration
layers; the first bounded implementation may traverse materialized objects.

Cache popular clone/fetch packs by repository identity, visibility/policy epoch,
generation or exact selected object-set digest, wants/haves, object format and
negotiated capabilities. Authenticate before every cache lookup and response.
Private data must never enter a public cache namespace; refs hidden by policy
must also be excluded from traversal and cached results. Keep the R2 bucket
private. Authorization revocation must take effect even for a previously cached
pack.

Keep a byte budget and expiry for durable download artifacts. A hot full-clone
pack may justify a second compressed copy; generating one for every ref rarely
does. Prefer artifacts assembled from existing chunks, deduplicate identical
snapshots, and evict cold artifacts. Cache misses always fall back to committed
storage. The Workers Cache API is local to a data center and is not durable or
globally replicated.
([Cache behavior](https://developers.cloudflare.com/workers/runtime-apis/cache/))

## Inline maintenance with a fixed budget

Append small segments at level zero. Combine a few similarly sized segments into
the next level; leave old, large history untouched by default. Reuse compressed
entries and eliminate duplicate IDs. This follows the useful principle of Git's
geometric repacking: merge only the small packs needed to restore the size
progression.
([Git repack](https://git-scm.com/docs/git-repack))

Every receive attempts at most one bounded maintenance step. Configure caps for
input/output bytes, decoded bytes, object count, delta-base reads, storage calls,
CPU and elapsed time. Persist progress at chunk boundaries. A candidate starting
budget is 8 MiB copied output with a low CPU deadline, subject to local results;
large-object validation has its own admission limits. Publishing a maintenance
manifest uses the same generation compare-and-swap as a push. If it loses a race,
retain reusable output and replan instead of changing refs.

Correctness requires finishing the accepted push's validation and publication.
Optional compaction can stop at its budget. Persist outstanding maintenance before
acknowledgment and continue small steps on later requests or a durable alarm.
Alarms provide retries, so steps must be idempotent. Do not use `waitUntil()` as a
durable queue. If incoming work outpaces bounded maintenance, reject or throttle
new writes at a documented backlog limit; silently accumulating unlimited tiny
segments is not a complete design.
([Durable Object alarms](https://developers.cloudflare.com/durable-objects/api/alarms/))

Readers obtain renewable generation leases before accessing a manifest. Retain
retired chunks until no live lease, retained snapshot, download artifact,
quarantine transaction or delta dependency references them, plus a grace period.
An expired reader must stop or reacquire and restart. Garbage collection marks
from current/retained roots and active leases, then sweeps after a second check;
never delete merely because a newer manifest exists. Reflog/retention policy also
protects recently removed refs. Failure to renew a lease must not be masked by a
cache hit.

## Cloudflare feasibility limits

As checked on 2026-10-17, Workers has 128 MB memory per isolate, paid CPU budgets
up to five minutes, six simultaneously opening outbound connections and a
default 10,000 paid subrequests per invocation. Request-body limits depend on the
zone plan: 100 MB Free/Pro, 200 MB Business, 500 MB Enterprise by default. Streaming
does not remove the upload limit. A Linux-sized initial push cannot be assumed to
fit ordinary smart HTTP on a default zone. Use an explicit staged import path or
plan for a verified limit increase; do not claim transparent unlimited pushes.
([Workers limits](https://developers.cloudflare.com/workers/platform/limits/))

Use a small concurrency cap, backpressure and per-repository memory admission.
Keep initial whole-pack limits well below the isolate ceiling because input,
inflated bases, indexes and output can coexist. The local Rust runner can invoke
native Git for its first codec adapter. Workers cannot run those subprocesses;
the Worker path needs portable Rust inflation, delta resolution and pack output
with equivalent validation and budgets. The existing Rust core's stored-block
encoder alone does not provide compressed, delta-preserving hosted packs.

Implementing Cloudflare wrappers and passing local service tests will not prove
cloud latency, resource use, storage durability or deployment compatibility.
Those need a later bounded cloud verification after local correctness and
performance gates pass.

## Linux-sized cost model: assumptions, not a measurement

No Linux clone is required for this estimate. The scenarios below are
**hypothetical Linux-sized histories**, not claims about the current kernel's
object count or compression ratio. Replace these inputs with `git count-objects
-vH`, pack/index sizes and measured traffic when a real repository is available.
GB here is decimal; physical chunks below are 8 MiB.

| Assumption | Scenario A | Scenario B |
| --- | ---: | ---: |
| Git objects `N` | 10 million | 20 million |
| Raw canonical bodies `U` | 250 GB | 500 GB |
| Independent zlib bodies `L`, no delta reuse | 80 GB | 160 GB |
| Delta-compressed pack data `P` | 12 GB | 24 GB |
| Illustrative index, 64 bytes/object | 0.64 GB | 1.28 GB |
| Data chunks `ceil(P / 8 MiB)` | 1,431 | 2,862 |
| Data plus illustrative index chunks | 1,508 | 3,015 |

R2 Standard rates are $0.015/GB-month, $4.50/million Class A writes and
$0.36/million Class B reads, with monthly account allowances of 10 GB, one million
A and ten million B operations. Egress is free; billing rounds usage up to whole
units. These allowances are shared with other account workloads.
([R2 pricing, retrieved 2026-10-17](https://developers.cloudflare.com/r2/pricing/))

The following are our calculations, assuming the account allowances are otherwise
unused and steady storage for the month. Each loose object takes one PUT/GET;
each chunk takes one PUT/GET. Exclude manifests, extra index probes, retries,
multipart requests, temporary duplicates, Workers/DO compute and storage,
observability and taxes. They are **R2-only estimates**, not a hosting quote.

| R2 component | A | B |
| --- | ---: | ---: |
| Raw object bodies: monthly storage | $3.60 | $7.35 |
| Independently zlib-compressed bodies: monthly storage | $1.05 | $2.25 |
| Packed data plus illustrative index: monthly storage, rounded | $0.045 | $0.240 |
| One initial import, one PUT per Git object | $40.50 | $85.50 |
| One initial import, one PUT per data/index chunk | $0 | $0 |
| 100 cold full-history reads/month, one GET per Git object | $356.40 | $716.40 |
| Same 100 reads, one GET per data/index chunk | $0 | $0 |

For arbitrary monthly usage, apply the pricing page's rounding and remaining
account allowances to storage `S`, Class A count `A` and Class B count `B`.
Without free usage or rounding, the linear model is
`0.015*S + 4.50*A/1e6 + 0.36*B/1e6`. For `C` full-history reads,
object-per-key reads are `C*N`; chunk reads are roughly
`C*ceil((P + index_bytes)/chunk_bytes)` with separate data/index rounding. Sparse
fetches need a measured range-read amplification term instead.

At these assumptions, object-per-key storage is not ruinous on bytes alone. Its
initial writes, clone fanout, millions of storage calls and loss of cross-version
delta compression make it unsuitable for the intended hot path. A single clone
cannot put millions of reads into one bounded Worker invocation.

Rewriting the entire packed history after every push is also the wrong default.
One rewrite of scenario A copies about 12.64 GB, reads about 1,508 chunks and writes
about 1,508 replacements. At 100 pushes/day this is roughly 37.9 TB/month of copied
input and output each, before retries, while appending 1 MB/push adds only 3 GB in
the month. Compute and latency can dominate the small storage-operation bill.
Record physical bytes written divided by accepted new packed bytes as the write
amplification metric; bound it with levels and measure it over long push sequences.

Retired generations, quarantine and cached download packs increase peak stored
bytes. R2 storage accounting averages daily peaks, so a brief duplicate is not
automatically free. Model steady live bytes plus retained versions, maintenance
headroom and artifact budgets; retain Standard storage initially to avoid an
Infrequent Access retrieval/minimum-duration tradeoff.
([R2 storage accounting](https://developers.cloudflare.com/r2/pricing/))

## Local benchmark acceptance

The [hosting performance notes](hosting-performance.md) carry Rust runner commands
and measured results. Use
ordinary Git clients against both local HTTP endpoints. Compare against a bare
Git smart HTTP server with prepacked compressed history, reachability bitmaps,
commit graph, pack reuse and retained receive packs. Modern Git can reuse multiple
packs with a MIDX bitmap; it is not fair to compare only against an unpacked
default server.
([Git configuration](https://git-scm.com/docs/git-config))

Run initial push, incremental thin push, full clone, incremental fetch and
repeated warm fetch on evolving text, incompressible data, many tiny objects and
large objects. Report input and wire bytes, time to first byte, total wall time,
server CPU, peak memory, stored bytes, storage operation counts and write/read
amplification. Include maintenance debt and the time/bytes to clear it; delaying
work cannot disappear from the comparison.

Separate application-cache-cold, application-cache-warm and OS-page-cache effects;
record which layers were actually reset. Give Git matching filesystem cache and
durability conditions. Repeat trials, alternate server order, report dispersion
and pin machine/runtime/configuration details. Do not extrapolate a small
loopback result to Linux or Cloudflare, or call one configuration perfectly tuned.
If either send or receive loses, publish that result and the limiting profile
before changing the design.

Before cloud verification: run interoperability and integrity checks, competing
ref updates, malformed/truncated packs, missing delta bases, expansion limits,
restart after every publication boundary, budget exhaustion, cache isolation and
reader-versus-GC races. A performance claim requires correct reachable contents
and comparable durability, not merely a fast successful HTTP response.

## Certified branch discovery

Cold Cloudflare upload discovery can use manifest metadata after one complete
native protocol-v0 advertisement certifies the exact branch set, fixed main HEAD
and capability order. A bounded native type check also requires branch targets
to be commits. The learned native agent and capability template retain no refs
or objects. Strict native branch publications can advance the certificate;
recovery, external generations or changed codec/configuration invalidate it.
Tags, unborn repositories, other protocols and unrecognized requests or policies
keep the ordinary native path. Fetch POSTs retain native Git negotiation and
encoding; the bounded historical selection below can narrow their cold inputs.

`/metrics` reports advertisement shortcuts, native bootstrap attempts and the
one-time type-check count/time. `cache_bytes` and `cache_entries` describe the
existing response cache and clear after publication. The separate
`upload_advertisement_template_bytes` and `cache_retained_bytes` include the
bounded certificate in the same 32 MiB allocation budget. No certificate is
persisted, and eligible discovery writes no authoritative state or blobs.

### Ephemeral historical prefix views

Cold Cloudflare fetches may reconstruct a conservative subset of current packs
for a narrowly classified protocol-v0 upload POST. Warm views keep their existing
full reconstruction behavior. This candidate changes no durable manifest field,
SQLite schema/row, R2 object, physical chunk size, compaction budget, request/ref
limit, or benchmark/scorer/pricing input. It retains no object payload cache,
object map, old snapshot, duplicated ref names, or duplicated object IDs.

A private process-local certificate binds the authoritative generation, exact
ordered live pack descriptors (including representations, split lengths, levels,
and ordered chunk keys/lengths), and exact sorted current ref name/value pairs.
Retirement debt is deliberately excluded because deleting confirmed non-live
debt cannot change the live object universe. Existing full snapshots bootstrap
with floor zero for every pack and current-generation stamps for every ref;
known-empty generation zero starts with empty arrays. Restart, dirty/ambiguous
recovery, external generation, changed native policy/identity, and invalid binding
lose selectivity. No pack name, position, size, timestamp, or inferred ancestry
establishes provenance.

Only this process's strict confirmed native publication advances the proof. An
incoming self-contained pack gets the new generation as its floor. Compaction
transfers the **minimum floor of every selected input**, including duplicate-only
inputs and incoming packs, through the actual `CandidatePack` plan. An unchanged
exact ref name/value keeps its stamp; adding, changing, deleting/re-adding an
alias, or updating a ref without a new pack receives the new publication stamp.
The candidate proof is installed only after validated staged-upload/CAS and
retirement receipts, native ref replacement/pruning, and cache invalidation.
Errors or ambiguous replies drop it. A successful no-change receive may preserve
an exact unchanged proof without advancing it. Retirement and recovery always
use the full authoritative manifest.

The bounded classifier accepts only the exact upload POST path/MIME, identity
encoding, v0, the fixed native configuration/environment, branch-only refs, and
complete pkt-lines containing wants, a flush, ordinary haves, and terminal
`done` or flush. It requires at least one want, at most 128 KiB and 2,048 OID
commands, and an explicit ordinary capability allowlist. Every wanted and have
OID must exactly decode to a **current** published ref value. The maximum witness
across all commands and matching aliases selects every current pack whose floor
is no greater than that witness, in original order. Unknown OIDs, unsupported
capabilities/protocols/tags/shallow/filter/encoding/round state, or unsupported
provenance select Full before object I/O. An all-pack selection also uses Full.
This does not expose unadvertised OIDs or enable arbitrary-SHA1 wants.

Every selective read validates **all** refs, IDs/unique pack identities, sizes,
representations, chunk keys/lengths, repeated-key length agreement, split framing,
and total bounds, including omitted packs. It then consumes exact ordered
`VerifiedChunks` for selected packs. All selected payload lengths, SHA-256s and
request identity/order verify before **any** native file (including config/HEAD)
is created. Missing/corrupt selected data leaves Invalid and returns an error;
there is no hidden Full retry. Legacy separate pack/index files, bundles and
oversized scalar reads retain the shared verified storage path. Selected reads
authenticate consumed payloads; they do not promise to sweep unrelated omitted
payload corruption on every historical fetch.

The disposable native view explicitly records Full, Projection, or Invalid.
Projection writes only original current refs whose stamps are within its cutoff;
the fixed HEAD may be a dangling main symref in this POST-only view. Cache keys
bind the full authoritative generation and exact method/target/body/sorted header
map. A response cache hit leaves scope unchanged. Every cold miss performs real
selected R2 reads even when the same projection is already on disk. Same-generation
Full reads preserve valid lineage. Real receives establish Full before quarantine
alternates; unsupported native RPCs, native discovery fallback, and global object
inventory establish Full as well. Lightweight `/metrics` never restores it.
`/_object-stats` records its required actual Full restore before scanning every
authoritative pack, including when a cached latest response left a prior partial
view in place.

The optional certificate is capped at 32 KiB **inside** the existing 32 MiB joint
budget. The accounting includes owning struct sizes, array capacities, temporary
simultaneous old/new certificate storage, candidate-plan floor capacity, R8
advertisement templates and pending templates, response allocations and headers,
and the cache ring capacity/fixed bookkeeping. Opaque standard B-tree node
allocations receive a conservative per-header owning-allocation bound. Responses
are charged from the exact retained clone/key capacities so eviction subtracts
the same allocation charge. Optional metadata and ring growth reserve space
before allocation and evict through the existing response policy. `cache_bytes`
remains response-only (zero after a successful push); serde-default lineage,
pending-lineage, eviction and `cache_retained_bytes` fields report the joint
charge. View metrics expose selected/total pack counts and bytes and actual Full
restoration counts. No provenance is persisted.

The exact successful empty receive probe `0000` has an additional bounded cold
Cloudflare shortcut. After normal dirty recovery/authoritative generation checks,
complete metadata validation, exact path/MIME/v0/identity encoding, fixed native
policy identity matching every present lineage/advertisement certificate, no
transfer encoding, and absent-or-exact-4 content length, it returns the byte/header-equivalent native 200 empty receive result. It preserves
Full/Projection scope, both certificates, cache, generation, refs and retirement
debt. The next nonempty or ref-only push still reconstructs Full once. Unsupported
policy/framing/protocol/shape uses the ordinary strict path. In native Git 2.53,
`0000` yields FLUSH (`pkt-line.c:434`), `read_head_info` returns no commands
(`builtin/receive-pack.c:2167`), stateless mode suppresses advertisement, and no
commands bypass unpack/ref/hooks/GC/report work; the untouched sideband flag emits
nothing before success (`builtin/receive-pack.c:2647`). Direct native comparisons
cover unborn, nonempty Full and partial views. `receive_noop_shortcuts` counts
these operations; no unrelated payload authentication is claimed.

The explicit ignored Rust integration driver `history_cloudflare.rs` runs against
a separately started fresh local production Worker at port 8831. It performs real
registered gateway pushes and cold fetches, historical wants with/without haves,
a latest cache hit leaving Projection, no-I/O metrics, exact global-stat GET
restoration receipts, a default-client large streamed push after Projection
(including Git's real empty probe), strict fsck and identical source/client tar
archives. It asserts no read-side R2 writes/live/peak growth or SQLite growth.
The broad storage and fault tests still cover ordered chunk integrity, large
legacy scalar reads, retirement failure/lost replies and staged publication.

This is implementation/correctness preparation, **not a measured cost or speed
candidate**. The earlier R5 arithmetic projects fixed savings of 138 R2 GETs plus
15 callbacks ($0.05643 in its stated scenario); mandatory final Full inventories
add 24 GETs and eight callbacks ($0.01224), leaving $0.04419 in that separately
charged complete physical scope. Those are unmeasured conditional projections,
not full bills or CPU estimates. The benchmark's large `http.postBuffer` avoids
the empty receive probe, so that related shortcut changes none of this fixed
projection. Default-client probe savings are separate observed operations, not
scored workload savings. Full four-profile fresh screens and final paired
validation remain required; cache/latency/transfer/storage guards can reject the
candidate regardless of arithmetic.
