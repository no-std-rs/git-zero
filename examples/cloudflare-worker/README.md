# Cloudflare metadata as a Git repository

This demo wraps git-zero's allocator-free Rust Wasm server in a JavaScript
Cloudflare Worker. It serves a synthetic repository containing:

| File | Contents |
| --- | --- |
| `cf.json` | All serializable metadata available on the capture request's `request.cf` |
| `headers.json` | Public Cloudflare headers selected by `EXPOSED_CF_HEADERS` (default: `cf-ray`) |
| `vars.json` | Public string/JSON bindings named in `EXPOSED_VARS` |
| `runtime.json` | Demo platform and snapshot format information |

There is one deterministic commit with no parents, on `main`. Cloudflare does
not expose every internal variable or Wrangler's own process environment to a
Worker. Local emulation may supply different or empty `request.cf` metadata.
Bindings such as KV/R2/services are not serializable environment variables.

## Run locally

```sh
rustup target add wasm32-unknown-unknown
cd examples/cloudflare-worker
npm ci
npm run dev
```

Wrangler builds `git-zero-worker-wasm` with Cargo and bundles the resulting Wasm
module. Open `http://localhost:8787/`, copy `clone_url` from the JSON response,
and run:

```sh
git clone "$CLONE_URL" cloudflare-snapshot
GIT_SMART_HTTP=0 git clone "$CLONE_URL" cloudflare-snapshot-dumb
```

Set `CLONE_URL` to the returned URL before running those commands. Both protocols
serve the same tree. Deploy explicitly with `npm run deploy` when ready; setup
and tests do not deploy anything.

## Immutable snapshot URLs

`GET /` captures metadata once and returns `/r/<base64url-snapshot>/repo.git`.
The token contains the four UTF-8 JSON file contents. Every Git request rebuilds
that exact snapshot, even in another isolate or after a retry. Neither discovery
nor an object request captures new metadata. Visit `/` again for a fresh snapshot.

The default stateless mode needs no KV binding. Encoded snapshots are
public data and can appear in URLs/logs. Raw snapshot data is limited to 8 KiB;
request bodies are limited to 32 KiB. The Rust ABI has 64 KiB input and 128 KiB
output buffers. The optional storage modes below shorten snapshot URLs while
preserving the same repository contents and resource limits.

## Persisted snapshots

Set `SNAPSHOT_STORAGE` to `r2` and bind an R2 bucket as `SNAPSHOTS_R2`, or set it
to `kv` and bind a KV namespace as `SNAPSHOTS_KV`. For example, add this to
`wrangler.toml`, using your own bucket:

```toml
# Add these settings to the existing [vars] table.
SNAPSHOT_STORAGE = "r2"
SNAPSHOT_TTL_SECONDS = "86400"

[[r2_buckets]]
binding = "SNAPSHOTS_R2"
bucket_name = "your-git-zero-snapshots"
```

For KV, use `SNAPSHOT_STORAGE = "kv"` and replace the bucket block with:

```toml
[[kv_namespaces]]
binding = "SNAPSHOTS_KV"
id = "your-namespace-id"
```

`GET /` awaits the write before returning `/s/r2/<sha256>/repo.git` or
`/s/kv/<sha256>/repo.git`, plus an `expires_at` timestamp. The digest covers a
versioned record containing the four file contents and their expiry. Git
requests read that exact record, verify its digest and schema, and pin their
own snapshot before streaming. Changing the current metadata, variables,
retention setting or capture mode cannot change an existing URL's objects.
Identical concurrent records use the same key and bytes; no mutable latest-ref
pointer or isolate cache participates in repository identity. Storage bindings
must remain available for existing links even after switching capture modes.

Retention defaults to one day; `SNAPSHOT_TTL_SECONDS` accepts integer seconds
from 60 through 31536000. KV writes set a TTL. R2 records enforce their expiry on
read; configure a bucket lifecycle rule for the `snapshots/v1/` prefix to reclaim
expired data physically. Retention changes apply to newly captured snapshots.
Keep retention long enough for discovery, clone/fetch, and retries to finish.
An already prepared response remains valid if storage expires or is evicted
while its consumer is paused; later requests fail without recapturing metadata.

Storage is read through a stream capped at 8320 bytes, including record overhead.
Missing records return 404, records still present after their logical expiry
return 410, integrity/stream failures 502, and configuration/read/write failures
503, before any Git response bytes. KV physically deletes TTL-expired records;
these return 404, indistinguishably from eviction or a key that never existed.
The adapter retains no tombstones beyond the configured retention. Error
responses use `Cache-Control: no-store`; storage read errors include
`Retry-After: 60`. KV is eventually consistent: another location can temporarily
miss a completed write, and concurrent writes of the same key can be rate limited.
Retrying may be necessary; choose R2 when immediate cross-location availability
is required. Failed publication never returns a clone URL. A missing or corrupt
record never falls back to a fresh snapshot or to the request environment.

Snapshots remain public metadata. Apply deployment authentication (for example
Cloudflare Access) to both `/` and repository paths when access must be limited.
Storage keys and shorter URLs do not provide authentication. Public selection
controls apply to new captures; existing immutable URLs retain their original
files and object IDs.

The adapters use the documented [KV streaming read](https://developers.cloudflare.com/kv/api/read-key-value-pairs/),
[KV write/TTL](https://developers.cloudflare.com/kv/api/write-key-value-pairs/), and
[R2 Workers APIs](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/).

The Worker instantiates the precompiled module once per response. A ReadableStream
pull callback advances Rust only when the consumer requests bytes, copies each
chunk before reusing Wasm output, and forwards cancellation to Rust. Each instance
pins its own immutable snapshot and cursor across asynchronous pulls. Overlapping
responses own separate fixed buffers. There are no Wasm imports, Rust allocator, Rust
`std`, `wasm-bindgen`, or Rust Worker SDK dependencies. The JavaScript host uses
normal runtime allocation.

## Public metadata selection

Edit `[vars]` and the comma-separated `EXPOSED_VARS` list in `wrangler.toml`.
Wrangler string variables and secrets are indistinguishable through `env` at
runtime, so only deliberately selected public bindings are exported. Do not put
credentials in that list. The Worker accesses only named own bindings; missing
or undefined names are omitted. It copies plain JSON values rather than invoking
binding methods, getters or `toJSON`. Strings, booleans, null, finite numbers,
plain objects and dense arrays are supported. Unsupported selected bindings
(including KV/R2/service objects), cycles and accessor properties reject capture
with 503 before storage publication. Invalid request metadata returns 400.

Set `EXPOSED_CF_HEADERS` to a comma-separated public header list. Its default is
`cf-ray`; use an empty string to capture no headers. Names are case-insensitive
and must begin with `cf-`. All `cf-access-*` names and `cf-authorization` are
refused even when explicitly listed, because Access uses headers in that family
for credentials and authenticated identity. Authorization and Cookie cannot be
selected. Arbitrary custom credential headers must stay outside the public list.
Cloudflare documents the standard assertion header as an
[application token](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/).

Each selection is limited to 64 names and 4096 characters. Request metadata and
selected variable values share a 1024-value, 16-level JSON projection budget;
exceeding it returns 413. The existing 8 KiB serialized snapshot ceiling also
applies. Capture errors use `Cache-Control: no-store`, return no clone URL and
write no storage record. Reads never reapply the current selection to a stored
or embedded snapshot, preserving earlier Git objects exactly.

## Deployment authentication

The demo does not authenticate requests itself. To restrict a deployment,
configure [Cloudflare Access for the entire Worker](https://developers.cloudflare.com/workers/configuration/cloudflare-access/):
open Workers & Pages, select the Worker, choose Access, select Protect this Worker
behind Access, choose All traffic, configure the permitted policy and apply it.
Worker-level protection covers its associated domains, routes, `workers.dev`
hostname and previews. A hostname/path application protects only the URLs it
matches; capture `/` and both `/r/` and `/s/` repository routes need the same
access policy.

For unattended Git clients, configure a Service Auth policy and supply the
service-token client ID and secret on every HTTP request, including initial
capture and discovery/downloads. Cloudflare's
[service token guide](https://developers.cloudflare.com/cloudflare-one/access-controls/service-credentials/service-tokens/)
describes those headers. They are deliberately excluded from new snapshots.
Cloudflare enforces the deployment policy before invoking the Worker; merely
receiving an Access-looking header is not authentication. Local tests exercise
projection and Git interoperability without provisioning or emulating Access.

## Checks

```sh
npm test
npx wrangler deploy --dry-run --outdir /tmp/git-zero-worker-bundle
```

The tests exercise the actual compiled Wasm module, clone it with real Git over
smart and dumb HTTP, check `git fsck --strict`, and verify one commit, stable
snapshot IDs, variable selection, and overlapping requests. Direct ABI tests
enforce a 24-page (1.5 MiB) initial linear-memory ceiling and no memory growth
across repeated discovery/pack/loose requests, full-capacity inputs, malformed
frames and independent instances. The 64 KiB input and 128 KiB output capacities
are asserted separately; JavaScript allocation is outside this Wasm budget.
The raw ABI's `serve_with_format(length, method, format)` accepts format 0 for
SHA-1 and 1 for SHA-256; `serve` and the Worker use SHA-1. SHA-256 tests compare
independently computed snapshot IDs, pack trailers and loose objects, and reject
format mismatches while retaining the same memory ceiling.
The dry run validates
Wrangler packaging without deploying.

Storage tests use async KV/R2-shaped fixtures and fresh Worker handlers per
request, covering awaited publication, concurrent readers, eviction during a
paused response, cancellation, expiry, corruption, oversized records, and
read/write failures. Real Git clones/fetches each backend over smart v2 and dumb
HTTP concurrently, verifies every file against the captured record and runs
`fsck --strict`. These are local adapter tests; they do not provision or contact
a Cloudflare account or measure production storage latency.

References: [Workers Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/),
[request metadata](https://developers.cloudflare.com/workers/runtime-apis/request/),
[environment bindings](https://developers.cloudflare.com/workers/configuration/environment-variables/).

The host forwards `Git-Protocol` and `Content-Type` through the streaming ABI.
Smart discovery/fetch supports v0/v1/v2; the client chooses the version. Gzip
uploads are decoded with `DecompressionStream`, with both wire length and expanded
body limited to 32 KiB. The extended ABI frame adds two u32-LE lengths and the
Git-Protocol/Content-Type bytes after the target, before the body. Existing `serve`
and `serve_with_format` frames remain available for embedded callers that have
already validated HTTP metadata. Worker tests run real-Git v0/v1/v2 clones;
ABI tests cover SHA-1/SHA-256 commands and fixed memory without allocator imports.

`stream_start(length, method, format)` uses the extended HTTP input frame, validates
it, pins the snapshot and returns 3 bytes of response metadata in OUTPUT (u16-LE
status, u8 content-type code). `stream_read(capacity)` returns up to that many body
bytes in OUTPUT, 0 at EOF, -1 for invalid capacity and -2 for invalid/failed state.
Capacities must be 1 through `output_capacity()`. Copy successful bytes before
pulling again. `stream_cancel()` releases the session. A new start, a legacy serve
call, or `input_ptr()` cancels the old session before buffer reuse. Never mutate
INPUT during a live session; calls are synchronous, single-threaded and
non-reentrant. Failed starts leave no old session; read failures cancel and the
host aborts the response. A prepared response can exceed OUTPUT's capacity.
Requests are still accumulated within the fixed INPUT limit before starting.
