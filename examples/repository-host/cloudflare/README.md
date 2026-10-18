# Repository hosting with local Cloudflare bindings

Run real workerd/Miniflare R2 and SQLite Durable Object bindings locally. A small
JavaScript Worker proxies Git smart HTTP to the Rust/native Git codec, which calls
back through the same Worker's service adapter. Test and benchmark runners are
Rust. No Cloudflare account, remote binding or deployment is needed.

```text
Git client → local Worker gateway → Rust Git codec
                                  ↓
             Worker service adapter → repository Durable Object → R2 / SQLite
```

The Durable Object serializes service operations and atomically publishes the
manifest in SQLite; compressed chunks live in R2. The gateway waits for Rust
outside the Durable Object, so callbacks can acquire the repository object.
The local Rust view is a disposable cache, not authoritative storage. This is
a working hybrid service adapter; Git decoding still runs outside workerd.

## Run

From the workspace root, install the pinned Wrangler and build the Rust binaries:

```sh
npm ci --prefix examples/repository-host/cloudflare
cargo build --profile hosting -p git-zero-host --bins --locked
```

Start the local emulator in one terminal:

```sh
WRANGLER_LOG_PATH=/tmp/git-zero-wrangler-logs \
WRANGLER_HIDE_BANNER=true WRANGLER_SEND_METRICS=false \
X_LOCAL_EXPLORER=false X_LOCAL_OBSERVABILITY=false \
node examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js dev \
  --config examples/repository-host/cloudflare/wrangler.toml --local \
  --ip 127.0.0.1 --port 8798 --persist-to /tmp/git-zero-cloudflare-state \
  --show-interactive-dev-session=false
```

An existing checkout can reuse
`examples/cloudflare-worker/node_modules/wrangler/bin/wrangler.js` instead of
installing another copy. Wrangler 4.149.0 is pinned. The environment variables
disable update banners, telemetry and optional local observability sidecars, and
keep logs out of the home directory. All bindings show `local` at startup.

Start and automatically register the codec in a second terminal:

```sh
target/hosting/repository-host --cloudflare http://127.0.0.1:8798 \
  --repo demo --token git-zero-local-services --view-cache warm \
  /tmp/git-zero-codec-demo 8910
```

The same fixed codec port can register again after a restart. To switch ports,
run the same command with `--unregister` instead of the local root and port first.
After restarting the emulator, restart the codec to register its origin again.

Push any ordinary repository through the Worker, then clone it:

```sh
git -C /path/to/source push http://127.0.0.1:8798/demo/repo.git HEAD:refs/heads/main
git clone http://127.0.0.1:8798/demo/repo.git /tmp/git-zero-cloudflare-clone
```

Open <http://127.0.0.1:8910/_object-stats> for object statistics, binding request
counts, storage inventory and cost estimates. Those diagnostic requests remain
separate from Git workload accounting. `/health` on the Worker reports readiness.

## Measure

With the emulator running, use the Rust runner's fresh repository namespaces:

```sh
target/hosting/hosting-bench --cloudflare-endpoint http://127.0.0.1:8798 \
  --cloudflare-token git-zero-local-services --cloudflare-repo-prefix localbench \
  --view-cache cold --samples 5 --fixture-mib 4 --pushes 32 --random-fetches 16 \
  --output /tmp/git-zero-cloudflare-benchmark.json
```

`warm` retains the derived native pack view; `cold` rehydrates published chunks
so reads issue actual R2 GETs. Report the selected cache policy with measurements.
Loopback emulation measures local overhead and real binding operations, not cloud
latency, Worker CPU billing or production throughput. The [hosting design](../../../docs/hosting-design.md)
and [native measurements](../../../docs/hosting-performance.md) explain the wider scope.

## Service contract and accounting

Internal routes require `x-hosting-token` and `x-hosting-repo`; repository names
use 1–80 letters, digits, underscores or hyphens. R2 keys are prefixed by repository.
The checked-in token is a local development gate. Use one codec writer per repo.
Registering a different codec origin returns 409 until explicitly unregistered.

| Route under `/_services/` | Method | Behavior |
| --- | --- | --- |
| `register`, `unregister` | POST | Manage a loopback codec origin for this namespace |
| `blob/chunks/<sha256>` | GET / PUT / DELETE | Immutable compressed bytes; PUT verifies content hash |
| `blobs/read` | POST | Ordered bulk GET of `{chunks:[{key,len}]}`; binary response, one binding GET per entry |
| `manifest` | GET / POST | Load state; CAS `{expected,next}` returns `{published}` |
| `manifest/retire` | POST | Opt-in v1 CAS and bounded ordered retirement, described below |
| `manifest/upload-retire` | POST | Opt-in v1 binary final-chunk upload, durable CAS, then ordered retirement |
| `gc` | POST | `{expected_generation,live_keys}`; stale generation returns 409 before deletion |
| `metrics` | GET | Inventory, actual operation counters and per-phase accounting |

New codec registrations also request `manifest_retire: 1`; supporting Workers
return `manifest_retire: {version: 1}` in that existing registration response.
Absent or unknown versions use ordinary CAS and scalar cleanup before attempting
publication. More than 256 debt keys also select scalar cleanup before mutation.
There is no additional capability request on a push. The original `publish` API
remains CAS-only; `publish_retiring` orchestrates either the fused operation or
the default/local CAS followed by scalar deletion under exclusive host ownership.

`POST manifest/retire` accepts `{version:1,expected,next}`, within the existing
1,500,000-byte JSON bound. The Worker finishes buffering this small body before
the DO gate. Before mutation, the DO validates generation, debt bounds, duplicate
debt and pack IDs, content-addressed keys, lengths and complete pack representations.
Eligible keys retain manifest debt order and exclude all live `pack`, `index`
and `bundle` chunks. Stale CAS returns
`{version:1,published:false,retirement:null}` with zero retirement/R2 I/O.

After atomic full-manifest replacement, publication storage sync completes and
`manifest_cas` records success. Only then does the DO sequentially invoke the
existing scalar removal logic, with the same actual R2 deletes, catalog SELECT
and DELETE statements, inventory updates and operation accounting. Per-key
failures do not stop later keys or change successful CAS accounting. A final
catalog storage sync precedes
`{version:1,published:true,retirement:{generation,outcomes:[{key,deleted}]}}`.
`deleted:false` retains retryable debt, and a failed R2 delete remains an actual
`r2_delete.failures` counter that existing benchmark validation rejects.

The durable manifest keeps the complete garbage list, without another manifest
SQL update. Rust validates the exact generation, key order, completeness and
boolean outcomes within a 32 KiB reply bound; the Native host clears only
confirmed debt in memory and never collects the same receipt twice. After a
fused request is issued, a 404, malformed response, lost reply, transport error
or final sync error is an error. The already-dirty native view must recover from
authoritative state before its next request; it never republishes or silently
falls back after an ambiguous attempt. Startup/dirty recovery still reconciles
orphans and the catalog while preserving every live representation. The existing
single Native owner contract, storage/cache budgets and compaction policy apply.

A registration may also request `manifest_upload_retire: 1`. The exact reply is
`{version:1,wire_bytes:16779276,metadata_bytes:1500000,chunk_bytes:16777216,retirement_keys:256}`
under that field. Absent/unknown versions or different bounds disable this
independent capability. Registration failure and unregister clear prior local
capabilities; no probing request is added to a push.

Cloud import explicitly stages only the final new chunk through `StagedPack`
and privately constructed `PendingChunk`. Ordinary `BlobStore::put`, `store_pack`
and `store_bundled_pack` remain eager/durable, and local native hard-link imports
remain eager. Staging reads a virtual pack/index concatenation into one bounded
chunk, rather than duplicating a whole bundle. Before importing a subsequent new
pack, the host durably flushes the prior pending tail; unchanged packs do not
flush it. There is at most one pending 16 MiB tail per receive operation, and no
pending payload enters SQLite or the response cache.

`POST manifest/upload-retire` carries `GZUP`, little-endian u32 version 1 and u32
metadata byte length, then UTF-8 JSON `{version:1,expected,next,chunk:{key,len}}`,
then exactly `len` raw bytes and EOF. The whole frame retains the existing
16 MiB + 2,060-byte wire ceiling; metadata is at most 1,500,000 bytes, the nonempty
chunk at most 16 MiB, and debt at most 256 keys. Metadata consumes wire space.
A full final chunk with too much metadata therefore uses eager fallback before
any fused request: exactly one ordinary durable PUT, then `publish_retiring`.
Unknown capability and out-of-bounds debt likewise select fallback before I/O.

The gateway finishes the bounded upload before the DO gate. The DO consumes and
validates the complete frame, exact/duplicate JSON fields, manifest shape and
bounds, refs, matching live chunk membership and SHA-256 before mutation. It
executes the same real PUT/catalog/inventory/high-water helper and storage sync,
then the existing transactional CAS and sync, then ordered retirement and final
sync. It adds no HEAD, generation read or per-reference probe. Stale CAS leaves
a durable upload and performs zero retirement: only authoritative GC may decide
whether that upload is an orphan, since a winning manifest can share its key.

The required <=32 KiB response is
`{version:1,uploaded:{key,len},published,retirement}`. Stale publication requires
explicit `retirement:null`; success requires the exact generation and complete
ordered outcomes. Rust rejects unknown, missing, duplicate, malformed or trailing
fields and mismatching upload metadata. The fused request also requires the
existing valid per-RPC operation-usage receipt. After issuing the request, every
HTTP/transport/receipt error remains an ambiguous error: no scalar fallback or
second publication occurs, and native dirty recovery runs before later Git work.

New codec registrations opt into `generation_witness: 1`. The authenticated
registration response supplies a private random 32-byte HMAC key and random
session; only the registering host's shared HTTP adapter retains them. Neither
appears in metrics or benchmark reports. Registration at the same origin rotates
both; a different origin still returns 409. Legacy registrations and
`CODEC_ORIGIN` forwarding perform no gateway manifest prefetch. A new codec
registered with an older Worker receives no capability and uses ordinary callbacks.

For an opted-in codec, the existing gateway invocation reads the actual DO
manifest and merges its normal operation receipt before forwarding Git. It signs
an HMAC-SHA256 witness over a versioned JSON array containing the session,
repository, HTTP method, exact forwarded path including query, and safe-integer
generation. The canonical ASCII JSON array is
`["git-zero-generation-witness",1,session,repository,method,target,generation]`.
The header `x-git-zero-internal-generation-witness` carries
`1:<session>:<generation>:<64 lowercase hex MAC characters>`. The gateway removes
every incoming `x-git-zero-internal-*` header before setting its own compact
witness. Rust uses the standard `hmac` crate's constant-time verifier. A valid matching generation skips only the dispatch
manifest callback. Missing, malformed, forged, unsupported, old-session or
mismatched-generation proofs use the existing authoritative load and recovery.
Dirty-view recovery always runs first, and a prefetched manifest never seeds
recovery. CAS, garbage collection, chunk storage and cache budgets are unchanged.
`generation_witness_hits` and `generation_witness_fallbacks` expose native outcomes.

The actual DO request, manifest GET and SQL remain counted; the hypothesis removes
one Worker callback per matching Git HTTP request. Prefetch/receipt/signing errors
fail the gateway request, and missing receipts still mark accounting incomplete.
Git request bodies continue streaming without another body buffer. The witness
relies on this experiment's trusted loopback gateway-to-codec channel; it proves
an authority read for the forwarded request, not its body or production identity.
An earlier queued proof whose generation differs after publication forces a fresh
load. Matching proofs place the authority read at the gateway DO read, with the
existing publication CAS preserving concurrent-write checks.

Blob bodies are limited to 16 MiB; the native Cloudflare adapter writes 16 MiB
chunks. This is a host storage policy: the generic repository default remains
8 MiB. Existing 8 MiB manifests remain readable alongside new 16 MiB manifests.
The independent compaction budget remains 8 MiB, four inputs and one job per push;
a pack larger than that budget is never pulled into an inline rewrite.
New Cloudflare packs concatenate the pack and index into `bundle` chunks, with
`pack_bytes` marking the split. Recovery verifies all complete chunks before
reconstructing both files. Manifests with separate `pack` and `index` chunks remain
readable; a pack cannot mix the two representations. Local storage retains separate
files so single-chunk packs and indexes can share immutable blob inodes.
Snapshot rehydration batches across packs, with no retained chunk cache. Each batch
allows at most 256 strict `chunks/<64 lowercase hex>` keys, positive lengths up to
16 MiB, a 1.5 MB JSON request, and 16 MiB of payload. The separate wire limit is
16 MiB + 12 + 8 × 256 bytes (16,779,276 bytes). The frame
is `GZBR`, a little-endian u32 version (`1`), a u32 count, then a u64 byte length and
raw bytes per requested entry. Its exact allocation is 12 + 8 × entry count +
payload bytes, allowing one full 16 MiB chunk or two full 8 MiB chunks.
Repeated keys retain one actual R2 GET per occurrence. Valid legacy chunks larger
than 16 MiB use scalar GETs within the existing 128 MiB Rust reader bound. The
Rust reader rejects bad framing, lengths and SHA-256 hashes before materializing
pack files; a failed read leaves the native view dirty.
The Worker finishes bounded bulk JSON intake before forwarding to the Durable
Object; oversized uploads return 413 with one Worker request and no DO dispatch.
Chunk and reconstructed file payloads each stay within the existing 128 MiB
snapshot limit, with at most one bounded bridge response in flight. The Worker
holds up to 16 MiB of chunk payload alongside the exact frame allocation; the
Rust reader holds the frame alongside verified payload copies. These bounds do
not measure total runtime memory or prove a 128 MiB isolate bound: ordinary PUTs
can now contain 16 MiB, and hashing, binding and runtime copies remain unmeasured.
Request (64 MiB), view (128 MiB), and response cache (32 MiB, 256 entries, at most
16 MiB per entry) budgets stay unchanged. Frame version 1 is unchanged: older
clients work with the newer Worker. Update the client and Worker together; the
16 MiB writer requires the Worker's 16 MiB bulk-entry limit. An empty
snapshot issues no blob RPC. Failed batches retain receipts for completed binding
attempts and their actual bytes; an uncertain partial body read marks accounting
incomplete through the existing missing-receipt path.
The 16 MiB layout is a prepared cost candidate, not a measured performance result.
Larger chunks can reduce binding operations without changing pack/index payload
bytes; content-addressed deduplication, storage peaks and full trace operations
must still pass paired benchmarks with unchanged fixtures and cache policies.
Projected request savings do not establish billed compute or a full hosting bill.

Manifest request JSON is capped at 1.5 MB to stay below SQLite's row/value limit;
larger repositories need paged metadata. Startup/orphan GC assumes exclusive codec
ownership: generation checking alone cannot protect another writer's in-flight
unpublished chunks. GC and metrics never pass through the Git gateway.

Counters record actual R2 PUT/LIST (Class A), GET/HEAD (Class B), DELETE, bytes and
SQL cursor rows, including failed binding attempts. Observer inventory LISTs and
SQL reads are tagged `stats`; workload cost subtracts that phase explicitly.
Gateway, service-bridge and observer Worker requests are counted separately.
`r2_key_access` shows ten active keys from a bounded 256-key recent-access map,
with binding attempts and bytes. It records chunk use, not individual Git object
reads inside native Git. Eviction sets `truncated`; aggregate counters retain all calls.
These extra HTTP bridge calls belong to this hybrid topology, rather than a
future portable codec's unknown request count. CPU time and DO GB-seconds remain
unknown when not measured. The Worker sums validated per-RPC usage receipts,
including constructor SQL, so aggregate counters survive ordinary DO eviction
without checkpoint writes or disabling eviction. `counter_epoch` identifies the
Worker runtime; `do_instance_epoch` separately tracks DO reconstruction. Worker
restarts reset these runtime counters; R2 inventory, manifest and high-water state
persist. Missing/invalid receipts or uncertain dispatch failures set
`accounting_complete:false`; cost estimates and benchmark deltas reject that trace.

## Retirement failure contract

The ordinary ignored Rust contracts `real_fused_retirement_*` compare fused and
scalar callback counts and identical R2/SQL work, reject malformed/bounded input
before CAS, and preserve all live chunk representations. The two-phase
`seed_before_workerd_restart` / `verify_after_workerd_restart` contracts also keep
partial durable cleanup debt through a real process restart alongside mixed
8 MiB and 16 MiB layouts, then validate native fsck, shared chunks and GC.

A separate runtime fixture, `test/retirement-fault-worker.js`, wraps the real R2
binding for a single named chunk. It passes one invalid Symbol argument to
`R2Bucket.delete`, so the actual binding rejects that call inside the production
operation wrapper. This tests local binding failure accounting; it does not
simulate a remote storage/network outage or modify counters. No production route
contains a fault hook, and the test runner remains Rust.

Start a separate local Wrangler with `--config
examples/repository-host/cloudflare/test/retirement-fault.toml`, a fresh
`--persist-to` directory, and your chosen local port. Then run:

```sh
GIT_ZERO_CLOUDFLARE_ORIGIN=http://127.0.0.1:8824 \
GIT_ZERO_CLOUDFLARE_TOKEN=git-zero-local-services \
cargo test -p git-zero-host --test cloudflare faulted_fused_retirement \
  -- --ignored --test-threads=1
```

The contract requires three retirement attempts, two successes, one genuine
failed binding call, successful durable CAS and retained failed debt/blob. It
then retries cleanup idempotently. The Native Rust HTTP fixture separately
verifies partial-outcome handling and a lost reply after committed publication,
including dirty recovery before the next read and absence of duplicate CAS.

## Final-upload failure and restart contracts

The `real_upload_*` Rust contracts compare the new operation with eager PUT plus
fused retirement, require identical real R2 bytes and SQL rows, exercise stale
shared keys and orphan GC, and validate exact framing and eager overflow fallback.
The registered native bundle and gateway contracts cover publication, fetch,
compaction, codec restart, archive/objects and strict Git fsck.

`test/upload-fault-worker.js` is a test-only wrapper used with
`test/upload-fault.toml`. The Rust `faulted_upload_barriers_*` contract checks real
binding failures, interruption between PUT and catalog upsert, both durability
barriers, partial deletion and lost/corrupt committed receipts. The separate
`faulted_native_upload_lost_response_*` contract verifies authoritative recovery
without another PUT or CAS. The wrapper adds no production fault hook or runner.

For a production-config process restart, run `seed_upload_before_workerd_restart`,
stop/restart Wrangler with the same persist directory, then run
`verify_upload_after_workerd_restart`, using one stable
`GIT_ZERO_CLOUDFLARE_RESTART_REPO`. This combines legacy 8 MiB and new 16 MiB
layouts, fused publication, a durable stale-upload orphan and shared live debt.
For catalog interruption across restart, use the upload-fault config, a stable
namespace containing `-upload-catalog-`, and the two Rust contracts
`seed_upload_catalog_before_workerd_restart` /
`verify_upload_catalog_after_workerd_restart` in separate Wrangler processes.
All contracts are ignored by default and require explicit local endpoint/token.

New Native also requests `catalog_pages:1` with `catalog_peak:1`. Supporting
Workers opt into bounded durable catalog pages on the first validated mutation;
existing page stores remain readable by older Native clients after registry loss
or restart. Rolling Worker code back to a version that cannot decode pages is
unsupported. The [catalog-page contract](../../../docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r12-catalog-pages-candidate/contract.md)
records schema charges, ordering, recovery and the unchanged R11 crash limitation.
