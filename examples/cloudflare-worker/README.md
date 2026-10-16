# Cloudflare metadata as a Git repository

This demo wraps git-zero's allocator-free Rust Wasm server in a JavaScript
Cloudflare Worker. By default it serves a synthetic repository containing:

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

With the default flat layout, `GET /` captures metadata once and returns
`/r/<base64url-snapshot>/repo.git`.
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
read; configure bucket lifecycle rules for `snapshots/v1/` and, when tree capture
is enabled, `snapshots/v2/`, to reclaim
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

## Optional tree layout

Set `SNAPSHOT_LAYOUT = "tree"` in the existing `[vars]` table to opt into tree
captures. The default is `flat`. Both layouts use the same explicit public-header
and binding selections above; Access credentials remain excluded. The JSON
response lists the captured file paths and identifies the tree layout.

Each nonempty category becomes a directory. For example, `request.cf.colo`
appears as `cf/colo.json`, the selected `cf-ray` header as
`headers/cf-ray.json`, and a selected `PUBLIC_JSON` binding as
`vars/%50%55%42%4C%49%43_%4A%53%4F%4E.json`. A nested value remains one JSON file.
Empty categories are omitted and represent empty objects; the runtime category is nonempty.
This layout introduces no referenced empty-tree object, and its smart and dumb
clones pass complete object checks without an assisted import.

Keys use reversible UTF-8 percent encoding outside lowercase ASCII letters,
digits, underscore and hyphen, followed by `.json`. Uppercase letters are escaped
so case-distinct keys stay distinct on case-insensitive checkouts. Percent, dots
and path separators are escaped, so names cannot collide with their encoded
spellings or introduce path components. Reserved Windows device basenames escape
their first byte too: `con` becomes `%63on.json`, distinct from a literal `%63on`
key. Names must contain valid Unicode and
encode to at most 255 bytes including the suffix. Tree capture supports at most 56 files, the existing
8 KiB serialized metadata bound, and the raw ABI's 64 unique objects/128 KiB body
arena. Capture validates the repository before writing storage or returning a
URL; unsupported names or exhausted budgets return an error without truncation
or fallback to another layout.

All tree inputs, including externally supplied inline tokens and authenticated
storage records, admit at most 1024 JSON values with 16 levels per file value
before any pretty formatting. The 56-file and 8 KiB source limits are also
checked first, bounding formatting work and preventing compact nested JSON
from expanding beyond the host's admission budget before validation.

Tree snapshots explicitly bind version 2, layout and SHA-1 format. Inline URLs
are `/r/v2/<base64url-snapshot>/repo.git`; persisted URLs are
`/s/v2/{kv|r2}/<sha256>/repo.git`, using `snapshots/v2/` storage keys. Their records
retain the same awaited writes, expiry, digest checks and bounded reads as v1.
Every response imports its own immutable repository through the raw ABI.

Existing v1 inline/stored paths always retain the flat interpretation, their
original record bytes and Git IDs, independently of the current layout setting.
Changing capture layout also leaves existing tree URLs unchanged. Cross-version
payloads and malformed stored records fail explicitly. Storage bindings for old
URLs must remain configured; changing layout never recaptures metadata for a
download. Tree construction and validation add preparation work; Rust's memory
bound does not include JavaScript descriptors or backend allocations.

`tree.test.mjs` compares deterministic IDs, bodies and encoded names with Git's
independent `hash-object`, `mktree` and `commit-tree` constructors. Real Git
v0/v1/v2/dumb clones and fetches in `tree-worker.test.mjs` compare all objects,
refs, file bytes and strict fsck for inline, KV and R2 fixtures. Tests also cover
schema confusion, old IDs, Access exclusion, pre-write limits, corruption,
expiry, failed storage, delayed publication and concurrent captures. These are
local Node/adapter tests, without a Cloudflare deployment claim.

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
SHA-1 and 1 for SHA-256; `serve` and the default metadata Worker use SHA-1. SHA-256 tests compare
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

## Importing bounded repositories

`src/repository.mjs` exports `createRepositoryWorker(module, loadRepository)` for
hosts that already have canonical Git objects. Its trusted asynchronous loader
receives the Request and returns `{ repository, target }`, or `null` for no route.
The loader owns authentication, routing, storage and immutable URL policy. It
must select the same repository across discovery and object downloads. This
adapter does not change the default metadata handler, its bindings, or existing
snapshot URLs.

The repository descriptor has a `format` of `sha1` or `sha256`, an `objects` array
of `{ id, kind, body }` records, a `refs` array of `{ name, id, symbolicTarget?,
peeled? }` records, and a `head`. IDs are lowercase hexadecimal; kinds are
`commit`, `tree`, `blob` or `tag`; bodies are uncompressed canonical `Uint8Array`
values, without the loose-object header. HEAD is `{ type: "symbolic", target,
id }`, `{ type: "detached", id }`, or `{ type: "unborn", target }`. Refs must
follow the core validator's ordering and name rules. Supply exactly the unique
objects reachable from HEAD and refs; import validates IDs, graph edges, symbolic
refs and peeled targets before preparing Git output.

Each instance owns at most 64 objects, eight refs, 128 KiB of raw object bodies,
4 KiB of copied HEAD/ref names and an 8 KiB manifest. Object bodies arrive through
repeated chunks in the existing 64 KiB INPUT, then live in a separate fixed arena.
The 128 KiB OUTPUT, 32 KiB wire/decoded Git POST bound and legacy frame remain
unchanged. Validation uses 64 object slots and 512-byte read/parse scratch;
streaming uses bounded control/selection storage. Long structural lines or
selection limits can reject a repository/request even when its total bodies fit.
JavaScript descriptors and backend reads are outside the Rust memory budget.
There is no asynchronous Rust provider wait during a stream pull.

The loader completes before an independently owned Wasm instance imports its
repository. Loader failures return 503, invalid sources 502, source capacity
failures 413 and invalid request intake 400, with `Cache-Control: no-store` and
without backend error details. Successfully prepared responses use the same
pull/cancel adapter as metadata snapshots.

The additional exports are `repository_capacity()`,
`repository_manifest_capacity()`, `repository_begin(length)`,
`repository_write(length)`, `repository_finish()` and
`stream_start_repository(length, method)`. Begin/write read INPUT; finish checks
the exact byte count and authenticates every object. Import operations return 0,
-1 for insufficient capacity, or -2 for invalid data/state. A begin cancels any
response and invalidates the previous import, including a failed begin. Writes
and finish require an unfinished import; rejected calls on a ready import leave
an active response intact. A request start requires a finished import and pins it
until cancellation or EOF. `input_ptr()` cancels a response but preserves the
finished import. Calls remain synchronous, single-threaded and non-reentrant;
never mutate INPUT during a live response.

The versioned manifest starts with the four bytes `GZR\0`, then little-endian
u32 version 1, frame kind 1 (raw objects), format (0/1), object count, ref count
and HEAD kind (0 unborn, 1 symbolic, 2 detached). A non-unborn HEAD contains its
raw ID; a non-detached HEAD then contains its length-prefixed UTF-8 target.
Each object contains a u32 kind (1 commit, 2 tree, 3 blob, 4 tag), u32 body length
and raw ID. Each ref contains a length-prefixed name, raw ID, length-prefixed
symbolic target (empty when absent), u32 peeled flag (0/1), and optional raw
peeled ID. IDs occupy 20 or 32 bytes according to the format. All lengths are
u32 little-endian. Trailing manifest bytes are rejected. Writes concatenate the
object bodies in descriptor order. The request-only frame is a length-prefixed
target, two header lengths, Git-Protocol/Content-Type bytes and remaining body;
its format comes from the imported repository.

`test/fixture.py` independently builds deterministic Git fixtures in both formats
and reuses the project's raw exporter. Each has 44 objects (10 commits, 19 trees,
13 blobs and two nested tags), seven refs, a merge, eight directory components
(nine tree levels including root), executable/symlink/empty modes and a 96 KiB
binary. Raw bodies total 102,426 bytes for SHA-1 and 103,362 for SHA-256. Real Git
v0/v1/v2 full and incremental clones compare refs, every canonical ID/body,
checkout bytes/modes and strict fsck with the Git-created source.

Loose dumb HTTP has an explicit empty-tree limitation: Git's walker omits the
referenced canonical empty tree from both this Worker and an unchanged Git-created
loose static server. Unassisted clones contain 43 of the expected 44 objects and
fail fsck for that one missing tree. Tests retain that result, verify the Worker's
direct empty-tree response and canonical ID, then explicitly import it before
checking all 44 objects and strict fsck. This is assisted coverage; packed dumb
publication must later provide the full unassisted closure.

Raw ABI tests also fill all 64 object slots, eight refs and the exact 128 KiB body
arena, stream responses larger than OUTPUT, reject excess input, and mix legacy
and imported sessions with cancellation and independent instances. The release
module currently starts at 22 Wasm pages (1.375 MiB), below the 24-page test
ceiling, and these tests assert no growth or allocator imports. This is a linear
memory result, not a measurement of JavaScript memory or deployed Worker CPU.


## Explicit promised-object authorization

Existing Worker starts and host defaults accept advertised IDs only. To allow
ordinary filtered clone checkout, a trusted `createRepositoryWorker` loader may
return `{ repository, target, wantPolicy: "all-visible-reachable", maxWantLines: 64 }`.
The policy covers every object kind reachable from that exact validated visible
HEAD/ref union, not arbitrary physical storage. The loader must authenticate and
project the repository first. Unknown policy values or missing optional Wasm
exports fail closed with 503. `maxWantLines` is an opt-in occurrence cap (default
64, unsigned 32-bit integer); advertised-only configuration retains its previous
behavior and ignores that option. Duplicate wants count too.

The metadata demonstration has the equivalent trusted deployment binding
`GIT_WANT_POLICY=all-visible-reachable` and optional `GIT_MAX_WANT_LINES` (default
64). Both flat and tree handlers use it consistently. Neither inline/captured
metadata nor a request header, query, capability or repository file grants it.
This setting exposes the complete already-public snapshot closure; it does not
replace deployment/route authentication or the public metadata allowlists.

Optional exports `stream_start_policy(length, method, format, policy, max_want_lines)`
and `stream_start_repository_policy(length, method, policy, max_want_lines)` accept
policy 0 (advertised IDs) or 1 (all visible reachable IDs). Existing exports remain
strict and work with old modules. Enabled metadata hosting requires both new
exports; imported hosting requires the imported export. Unknown/failed starts
cancel the previous cursor before returning an error. Policy lives only in the
prepared request, never in INPUT, the manifest, snapshots, URLs or later starts.
Strict restart, cancellation and independent instances do not inherit the grant.

V0/v1 enabled discovery truthfully adds `allow-reachable-sha1-in-want`; v2 uses
its ordinary fetch command. The same host option applies to discovery and POSTs.
A prior discovery is no access token. Hosts must retain immutable generations
if older promised blobs should remain available: tests distinguish a moving URL
that revokes old objects from a separately retained immutable URL. No retention
or GC guarantee is added by the policy itself.

The raw/input/output/object/ref/scratch/graph limits above stay unchanged, with
no growth within the 24-page Wasm ceiling. Large lazy batches can fail those
bounds. The occurrence limit is not a whole-request logical-work budget;
JavaScript/provider memory remains outside the Rust budget. Real Git acceptance
compares SHA-1/SHA-256 and v0/v1/v2 with Git's filtered HTTP backend: physical
blob omission, ordinary lazy checkout of the 96 KiB binary and other files,
history/side checkouts, exact blob:limit boundaries and canonical all-kind
selected pack inventories. Plain/gzip negative tests retain private/mixed-want,
projection validation, restart and old/incomplete-interface guards. These are
local bounded fixtures, not a deployment or throughput claim.
