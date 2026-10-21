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
request. Use ordinary clients:

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
