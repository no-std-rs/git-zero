# Repository host

This host implements Git smart HTTP in allocation-free `no_std` Rust Wasm.
Workerd supplies HTTP, SQLite and opaque R2 storage. Hosting never starts Git or
an external codec process. Git is required only for reference-client tests and
benchmark fixtures.

The deployment runs entirely on Cloudflare: clients connect directly to the
Worker, which runs the Git implementation with SQLite-backed Durable Objects
and R2 bindings. Discovery and pack transfers use that same endpoint.

From `cloudflare/`, build and test the real local bindings:

```sh
npm ci
npm run test:streamed
X_LOCAL_OBSERVABILITY=false npm run dev -- --var HOST_TOKEN:local-development-token
```

The test suite runs two files concurrently to bound the number of local
Wrangler/workerd processes. Each test retains its own workload and deadlines.

The default Wrangler config starts the own implementation with local R2 and
SQLite. The build script uses Cargo's `hosting` profile to optimize the codec
for throughput. The test and benchmark harnesses disable Wrangler's development trace
collector (`X_LOCAL_OBSERVABILITY=false`); it records every SQL operation and
can otherwise create a trace database larger than the repository metadata.
Pass `observability: true` to the test launcher when investigating traces.
Test-only routes are disabled in normal development and deployment.
Repositories are addressed by namespace, for example
`http://127.0.0.1:8787/demo.git`, and are created on their first authenticated
request in the default token mode. Use ordinary clients:

```sh
git -c http.extraHeader='Authorization: Bearer local-development-token' \
  push http://127.0.0.1:8787/demo.git HEAD:refs/heads/main
git -c http.extraHeader='Authorization: Bearer local-development-token' \
  clone http://127.0.0.1:8787/demo.git
```

`HOST_TOKEN` grants each namespace's complete published catalog, including
objects from earlier ref generations. Basic authentication
also accepts it as the password. Store a deployed token as a secret. For a new
SHA-256 repository, start with `--var REPO_FORMAT:1`; an existing repository's
format is fixed. Preserve the local Wrangler state across restarts.

For an embedding gateway, select `AUTH_MODE=gateway` and configure the
`GRANT_KEY` secret (at least 32 UTF-8 bytes of high-entropy key material).
The host verifies namespace-scoped HMAC-SHA-256 grants with read/write/admin
levels over both service bindings and plain HTTP. It sends no authentication
challenge in this mode. See the [HTTP contract and test vector](../../docs/repository-host-v1.md)
for exact minting, expiry and gateway header handling. Gateway repositories
require admin PUT creation at `/<namespace>.git/api/v1/repository`. The same
endpoint describes repositories and performs bounded, resumable deletion with
permanent namespace retirement. Token mode keeps implicit creation by default;
`IMPLICIT_CREATE=false` disables it. Read grants can browse refs, first-parent
history, trees, commits and safe streamed blobs under `api/v1/`; only objects
reachable from current refs are visible there. `api/v1/changes` provides durable
generation/time/actor/ref updates with resumable pages for reliable catch-up.
Admin `api/v1/policy` configures force permission and optional decoded-push/R2
payload byte caps (deployment ceilings still apply). Legacy size accounting can
be resumed in bounded POST pages.

For a product Worker, import `createRepositoryHost` from
`cloudflare/src/host.mjs`, pass your compiled codec and explicit binding names,
route prefix, auth mode and R2 key prefix, and re-export the returned Repository
class under your own name. Its fetch handler can serve a separate Worker, or a
Rust workers-rs router can mint a grant and call the stub directly without that
handler. The [v1 contract](../../docs/repository-host-v1.md#composing-the-host-in-another-worker)
defines the exact forwarded URL/namespace/grant shape, shared-bucket purge,
codec build pin, disabled-by-default health/test routes, memory controls and
per-repository admission. The reference `streamed.mjs` is only a thin deployment.
Clones, fetches, browse and private push stages run concurrently on one
repository. Only the ref compare-and-swap transaction is exclusive. Reads retain
consistent generation snapshots; delete fences, cancels live jobs and defers
purging until their I/O settles. Gateway defaults allow 32 active jobs plus
64 FIFO waiters (15-second admission wait), plus 64 cached-reader slots that do
not consume processing slots. A **64 MiB** namespace-fair isolate pool reuses
four codec arenas; idle arenas remain charged. Default fetches reserve 14 MiB,
pushes add control/report copies, and commit/tree/history/blob reserve
14/17/19/14 MiB respectively (12 MiB for a blob by oid). A cached reader needs 192 KiB and no codec.
A namespace holds at most two heavy codec leases, leaving another heavy slot
for a namespace with none and an arena for metadata. Fairness is per repository;
a team owning many namespaces gets many turns. Admission groups are not yet
supported, so gateways must enforce team-wide limits themselves.
One namespace cannot spend the last ordinary metadata allowance or all arenas;
waiters never preempt anyone's clone. Existing jobs retire naturally and larger
waiters cannot be overtaken indefinitely by new incumbent heavy jobs. Multiple
saturated namespaces can still receive retryable 503 after 15 seconds. Legacy
8 MiB chunks need an up-front allowance (up to 13 MiB extra); no holder waits for
a memory top-up. Share a ByteBudget and the same compiled codec module between
host factories. Budget the product's Rust/Wasm, V8, SQL, HTTP and GC alongside
these payload bounds inside 128 MB; see the contract's measured memory profile.

Identical full clones share a builder independent of the first client, with
fallback for pre-header builder failures and a progress-based build bound.
Cache capacity is reserved from a conservative catalog bound before preparation;
uncacheable clones prepare once each. An abandoned builder with no output for a
throughput window is cancelled. Mid-stream shared failures reach followers as
a Git-readable fatal sideband error requiring a new fetch.
Streams use 10 seconds idle, a 16 KiB/s floor over 30-second windows and **no
absolute lifetime**: 300 MB at 5 Mbit/s can finish in about 480 s. Control/JSON
input has a separate 120-second deadline starting after admission; pack input
uses minimum throughput instead. A 300 MB first push at 10 Mbit/s (~240 s) can
finish when its configured push cap permits it. Gateway push policy defaults to 256 MiB and the codec pack ceiling to
1 GiB: about 4.55/18.20 hours at the floor, excluding processing/I/O. Raising
caps extends that bound (64 GiB is roughly 48.5 days); configure rates and caps
together. Scratch contention gets retryable 503 before headers. Graph preparation
also uses a progress bound: the generated 25k-commit / 150k-object cold gateway
clone prepared in 20.7 s locally after batching private scratch writes, shared by identical followers rather than
repeated for each. Ref-rooted browser requests do not perform that closure walk.

Token mode is **exempt from the byte pool by default**, preserving its unlimited
pending queue, cache sizes, 60-second idle and lack of new body/admission/rate
bounds. This means no isolate-wide memory bound by default, plus up to four idle
codecs/factory (24 MiB allowance, 22.75 MiB actual linear memory). Explicit
isolateMemoryBytes/memoryBudget opts in; the default token cache profile needs
at least 100 MiB for one fetch, more for push controls, and lower caches for
concurrency. An insufficient pool is rejected. Enable
PACK_CACHE_ENABLED=true for repeated token-mode clones (gateway default: on).

Gateway fetches default to current-reachable wants. Rewritten objects remain
physically stored, including old IDs in the push log and historical ref
versions; force-pushing does not erase data. After a rewrite, non-tip raw-ID
fetches are refused while alarms rebuild reachability (about a minute in the
25k fixture). `wantPolicy: "catalog"`
opts into historic whole-catalog access (the token-mode default). There is no
unreachable-object garbage collector: complete repository DELETE reclaims its
content and permanently retires the namespace, including old packs and scratch.
Product copies/backups need their own erasure policy.

The current suite covers SHA-1/SHA-256 push, clone and fetch using Git protocol
preferences 0/1/2, atomic updates, push options, deletes, thin pushes, annotated
tags, `include-tag` in all three protocol versions, concurrent stale updates
and durable recovery. Depth-limited clones,
relative deepening, unshallowing, `blob:none`/`blob:limit`, `object:type` and `tree:<depth>` filters, and ordinary
lazy checkout work in both formats and all three protocol versions. Time-based
and exclusion-based deepening and sparse/combine filters remain unsupported.
Push and fetch accept streamed `gzip`/`x-gzip` request bodies; transport trailer
validation completes before received objects are published. Explicit tree and
tag wants retain their required non-commit closure when a client reports a
common commit, including the native filter exceptions for every wanted tag target.
Protocol v2 ref selection indexes up to 256 disjoint byte prefixes; duplicate
and contained prefixes reuse slots. Request bodies have a 60-second input idle
deadline that excludes chunk processing. Response readers have a 60-second idle
deadline, reset whenever output is pulled, so stalled clients release repository
scratch after cleanup without imposing a total transfer deadline.
Each HTTP operation also has shared Rust byte budgets: 1 TiB of canonical
output reservations, 16 TiB of decoding/representation work, and 64 GiB of
temporary canonical bodies. These accumulate across objects and repeated base
reconstruction, and exhaustion stays terminal for that request. Complete body
sizes are reserved before temporary writes; work counts input inflation,
representation reads and reinflated prefixes. `Codec` limit overrides can lower
or raise these defaults. Cleanup remains available after exhaustion. The local
state endpoint exposes `lastIntakeBudget`; this diagnostic resets on a cold
restart. The native Linux source inventory totals about 151.4 GiB of canonical
data; its 25 objects above the 8 MiB capture threshold total about 391.5 MiB.
This inventory is a compatibility check for the defaults, not a completed
hosting measurement.
Plain commits, trees and tags are scanned while their original decoded bytes
are available; their metadata stays private until pack and graph validation
finish. A large plain body does not require a temporary reconstruction copy.
The npm lockfile pins a runtime containing Cloudflare's
[RPC stream EOF accounting fix](https://github.com/cloudflare/workerd/commit/0edfb056c6fafb04a584cf23297b3de91bfcac7a).
Use `npm ci` to install that exact runtime; older builds can fail or truncate
compressed responses received over RPC.

Full Linux end-to-end validation remains incomplete; component decoder
measurements and local Wrangler runs do not establish deployed Linux-scale
hosting. Cloudflare's [request limits](https://developers.cloudflare.com/workers/platform/limits/)
apply to streamed HTTP bodies too. Request bodies are limited to 100 MB on
Free/Pro and 200 MB on Business; Enterprise permits self-service increases up
to 5 GB. Larger limits require agreement with Cloudflare. The measured initial
Linux push is 6,535,602,999 bytes, exceeding even that 5 GB limit. Streaming
does not bypass it. Workers also limit isolate memory to 128 MB; active CPU
per request is configurable up to five minutes on Paid. SQLite-backed
[Durable Objects](https://developers.cloudflare.com/durable-objects/platform/limits/)
limit each object's database to 10 GB. The current catalog uses one Durable
Object per repository. These limits still require a Cloudflare-only solution
before full Linux hosting can be claimed.

The [Wrangler configuration](cloudflare/wrangler.toml) is the production entry
point. With deployment access, create the configured R2 bucket, set `HOST_TOKEN`
using `wrangler secret put HOST_TOKEN`, then run `wrangler deploy`. The initial
deployment creates the SQLite-backed Durable Object class through its migration.
Verification for this task stays local; deployment has not been performed.

Direct workerd with the S3 fixture is retained only for local compatibility and
fault-injection tests. It uses workerd's experimental disk-backed Durable
Object API and an independent S3 signature verifier. The benchmark defaults to
actual local Wrangler R2/SQLite. Full runtime resource usage is measured by
`cloudflare/scripts/benchmark.mjs`; the 6 MiB Wasm and 48 MiB byte-cache bounds
do not describe the entire runtime process or deployed billing.
The push and cold-fetch reports also separate repository metadata on disk from
opaque object storage and local R2 mock metadata. Reference source/clone copies
are excluded. Production uses the Cloudflare R2 and Durable Object bindings;
local filesystem usage does not establish production storage cost.
New opaque chunks are 1 MiB, keeping more physical ranges in the fixed-size
read cache when delta reconstruction jumps between bases. Existing chunks up to
8 MiB remain readable; no stored pack or object offsets need migration.
Fresh catalogs store raw object-ID bytes and a numeric stage key in graph edges,
keeping each opaque stage UUID once in a separate lookup table. Existing
UUID/text and numeric/text catalogs remain readable and writable without
migration. In complete 100,000-edge SQLite fixtures, binary object IDs reduce
the numeric/text layout from 6,692,864 to 4,411,392 bytes for SHA-1 and from
9,703,424 to 5,795,840 bytes for SHA-256, with indexed reads retained. These
fixtures do not establish the full Linux storage reduction or hosting cost.
Each request also caches at most 256 aliases for verified delta-base metadata.
Missing and unresolved bases are looked up again as validation advances; the
metadata cache is released with the request's byte caches.

The mixed-workload runner salvaged from the earlier hosting experiment exercises
the current streamed host against Git's HTTP backend:

```sh
# From the repository root, after npm ci in cloudflare/.
node examples/repository-host/cloudflare/scripts/hosting-benchmark.mjs \
  --work-dir /tmp/hosting-mixed-warm \
  --runtime wrangler --view-cache warm --samples 2
```

Each pair executes 32 pushes and 56 latest/historical fetches. Fresh clients and
clients seeded with an older ancestor must receive missing history; strict fsck,
reachable-object lists and archive bytes are checked outside the wire timer.
Pushes update the main and historical refs atomically. The Git reference maintains
MIDX bitmaps and a split commit graph after each push; its ready time includes
that maintenance separately from wire time. Automatic maintenance is disabled
for this generated reference: its measured phase owns the MIDX/graph files, so
a detached repack cannot delete their inputs concurrently. Git's explicit
bitmap/graph optimizations and all correctness checks remain enabled. Pair order
alternates between samples.
The reference uses the test suite's threaded Python HTTP/CGI adapter; timings
include the different local frontends and do not isolate codec execution speed.
All pairs use one frozen host bundle; reports record its file sizes and SHA-256
digests as well as the Git version, fixture tips and codec commit. The generated
fixtures, frozen host bundle, logs and reports stay in the new
work directory, which the runner refuses to overwrite.

Use `--view-cache cold` to restart the own runtime before every fetch, or
`--object-format sha256` and `--protocol 0`, `1` or `2` for compatibility runs.
Cold startup counters remain visible before each operation; runtime construction,
client seeding and verification are outside wire time. OS page caches remain
uncontrolled. Smaller `--generations 4 --blob-bytes 4096 --samples 1` runs are
functional smoke checks. `--stage full` requires all 32 generations and at least
five pairs; a two-pair screen cannot establish a performance win.

Reports retain raw service counters and observer counters alongside workload
deltas. Inventory queries remain visible in totals but are subtracted from
measured SQL work. A changed repository-instance epoch, decreased counter or
incomplete counter schema fails the run. Unavailable SQL row counts stay `null`;
failed SQL and R2 attempts remain counted. Fetch guards allow scratch SQL writes
while requiring unchanged published/retained inventory and zero R2 puts/deletes.
R2 written bytes count attempted uploads, while read bytes count completed bodies;
partial failed response bytes are unmeasured. Worker CPU, Durable Object duration
and monthly SQLite storage remain unknown. These local measurements and the
recorded database size do not establish a complete cloud bill or deployed latency.

Ref/proof-rooted browse costs depend on returned rows, visited paths, directory
width and delta-chain depth, rather than unrelated history. Results carry
signed per-object `proof` tokens (plus commit tree/parent and peeled-ref proofs)
for immediate raw-ID navigation via `proof=`. They survive creations and verified
fast-forwards; rewrites/deletions revoke them. Other raw IDs use a bounded outward
index, advanced by requests and alarms; a rebuild may return a retryable 503
until proof is available. Tokens/cursors bind a random repository incarnation
and an HKDF-derived key. Read budgets can be raised within documented hard
ceilings; the scale fixture drives a default-packed, wide/deep 25k repository's
index to completion and compares results with Git.
