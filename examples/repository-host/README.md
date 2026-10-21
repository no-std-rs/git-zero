# Repository host

This host implements Git smart HTTP in allocation-free `no_std` Rust Wasm.
Workerd supplies HTTP, SQLite and opaque R2 storage. Hosting never starts Git or
an external codec process. Git is required only for reference-client tests and
benchmark fixtures.

From `cloudflare/`, build and test the real local bindings:

```sh
npm ci
npm run test:streamed
X_LOCAL_OBSERVABILITY=false npm run dev -- --var HOST_TOKEN:local-development-token
```

The default Wrangler config starts the own implementation with local R2 and
SQLite. The test and benchmark harnesses disable Wrangler's development trace
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
tags, concurrent stale updates and durable recovery. Depth-limited clones,
relative deepening, unshallowing, `blob:none`/`blob:limit` filters, and ordinary
lazy checkout work in both formats and all three protocol versions. Time-based
and exclusion-based deepening and tree/sparse/combine filters remain unsupported.
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

Full Linux end-to-end validation is running; component
decoder measurements do not establish a complete hosting result.

For large ordinary HTTP pushes, the same modules run directly under pinned
workerd with local SQLite and signed R2 S3 requests. See
[workerd.capnp](cloudflare/workerd.capnp),
[serve.mjs](cloudflare/scripts/serve.mjs), the systemd unit and TLS proxy example.
Set `HOST_TOKEN`, `R2_S3_URL` (full bucket URL), `R2_ACCESS_KEY_ID`,
`R2_SECRET_ACCESS_KEY`, and an absolute `GIT_ZERO_STATE_DIR`, then run
`npm run serve:standalone`. This runtime uses workerd's experimental disk-backed
Durable Object API. Its local tests use an independent S3 signature verifier.
Cloudflare's [request limits](https://developers.cloudflare.com/workers/platform/limits/)
apply to streamed HTTP bodies too; the direct runtime provides the large-push
ingress option with the same Git implementation and R2 storage API.

Verification for this task is local only. No live deployment, credential change
or infrastructure purchase is needed. Full runtime resource usage and hosting
costs are measured by `cloudflare/scripts/benchmark.mjs`; the 6 MiB Wasm and
48 MiB byte-cache bounds do not describe the entire runtime process.
The push and cold-fetch reports also separate repository metadata on disk from
opaque object storage and local R2 mock metadata. Reference source/clone copies
are excluded. In standalone production, object storage resides in R2; only the
repository metadata and runtime belong in the server's disk budget.
New opaque chunks are 1 MiB, keeping more physical ranges in the fixed-size
read cache when delta reconstruction jumps between bases. Existing chunks up to
8 MiB remain readable; no stored pack or object offsets need migration.
