# Repository host HTTP contract v1

This contract describes implemented routes in `examples/repository-host/`.
A gateway owns users, credentials, teams and repository-name mapping. The host
owns Git and trusts only cryptographically verified repository grants. HTTP and
Cloudflare service bindings have identical authentication semantics. Use TLS
outside local loopback. Do not forward grants to clients or log them.

## Configuration and identity

The thin reference deployment uses the variable/binding names in this section.
A composed deployment supplies its own names through createRepositoryHost below;
no binding, route or auth mode is implicitly selected by the library.

`AUTH_MODE=token` (also the reference unset default) preserves the existing `HOST_TOKEN`
secret: Bearer credentials or the password in HTTP Basic grant full access.
Invalid credentials receive HTTP 401 with the existing Basic challenge.
`AUTH_MODE=gateway` ignores `HOST_TOKEN` and requires the `GRANT_KEY` secret,
a UTF-8 string of at least 32 bytes. Generate a high-entropy key, unique to this
host deployment. Unknown modes fail closed. The reference `/health` reports auth readiness without
exposing configuration values. Store actual keys in deployment secrets, never
in a committed Wrangler `[vars]` section.

Namespaces are opaque single path segments matching `[A-Za-z0-9_-]{1,80}`;
UUIDs are accepted. A gateway maps its own `team/repository` identity to one of
these namespaces (an injective encoding or a stored identifier). It must strip
client-supplied grants and the internal `x-git-zero-namespace` header before
forwarding, and replace the grant with one minted from its own authorization
decision. Namespace identity comes from the URL; the Worker overwrites the
internal header and the Durable Object checks both again, including its own
binding-derived object identity. All route tables below describe suffixes under
the configured routePrefix (empty only in the reference deployment).

## Grants

Send the header `X-Git-Zero-Grant` with this ASCII value:

```text
v1.<namespace>.<access>.<expiry>.<actor64>.<mac64>
```

`access` is exactly `read`, `write` or `admin`. `expiry` is canonical decimal
Unix seconds (1–10 digits, no leading zero). The host accepts it only while
`now < expiry <= now + 300`; there is no clock-skew allowance. Mint for about
120 seconds with synchronized clocks. A request is checked at Worker entry and
again immediately on arrival at the Durable Object, before joining its job
queue. The verified access and actor are carried into the queued job; waiting
behind a clone, push, recovery or alarm does not expire an accepted request.
An accepted request and its response can finish after expiry. HTTP/service-binding
transport before Durable Object arrival still requires a valid grant: there is
no unverified timestamp header or transport trust shortcut.

The lower bound deliberately remains strict: an already expired grant is refused
on arrival. Both verification sites are on the host clock; a gateway should use
synchronized clocks and the documented 120-second minting lifetime (rather than
near-expiry grants). Queue waiting is handled by admission-time verification,
not by extending the replay window with clock-skew tolerance. The upper bound
also remains 300 seconds. The compatibility vector is unchanged. Grants are bearer capabilities, reusable until expiry within
their namespace and access level. They do not bind a body, HTTP method, nonce
or single operation. Use a separate host key per deployment to prevent replay
across deployments. Revocation requires waiting for expiry or rotating the key;
there is no persistent replay/revocation table.

`actor64` is unpadded RFC 4648 base64url of an opaque UTF-8 label, at most 128
bytes before encoding; empty is allowed. The host does not interpret it.
`mac64` is unpadded base64url of the 32-byte HMAC-SHA-256. Both encodings must
be canonical. The whole header is bounded to 512 characters.

The MAC key is the exact UTF-8 bytes of `GRANT_KEY` (no hex/base64 decoding).
The message is UTF-8 of the following string, with LF separators, including
the final LF; sign the encoded actor field, not its decoded text:

```text
git-zero-grant-v1\n<namespace>\n<access>\n<expiry>\n<actor64>\n
```

The version/domain separator prevents cross-protocol use. Signing the namespace,
access, expiry and actor prevents scope replay, privilege upgrade and tampering.
WebCrypto HMAC verification compares MACs in constant time. Token comparison
also uses HMAC sign/verify with a fixed public comparison key, avoiding direct
secret string comparisons. Neither mechanism relies on a trusted transport.

### Cross-language test vector

These are public fixture values, **not deployment credentials**:

```text
key (UTF-8): 0123456789abcdef0123456789abcdef
namespace: 550e8400-e29b-41d4-a716-446655440000
access: write
expiry: 2000000120
actor (UTF-8): user:42
actor64: dXNlcjo0Mg
message: git-zero-grant-v1\n550e8400-e29b-41d4-a716-446655440000\nwrite\n2000000120\ndXNlcjo0Mg\n
mac64: uFYwfIrtliIzA2H2Ypu6AZQJnjSJp4otWEMqop0kku0
header: v1.550e8400-e29b-41d4-a716-446655440000.write.2000000120.dXNlcjo0Mg.uFYwfIrtliIzA2H2Ypu6AZQJnjSJp4otWEMqop0kku0
verification clock: 2000000000
```

The `\n` notation in the vector denotes actual LF bytes, not backslash and `n`.
`test/grants.test.mjs` checks this vector using independent Node HMAC minting.
A Rust gateway needs only an HMAC-SHA-256 implementation, UTF-8 and unpadded
base64url; no JavaScript serialization or JSON canonicalization is involved.

## Git routes and access

| Method and route | Minimum access | Response content type |
| --- | --- | --- |
| `GET /<namespace>.git/info/refs?service=git-upload-pack` | read | `application/x-git-upload-pack-advertisement` |
| `POST /<namespace>.git/git-upload-pack` | read | `application/x-git-upload-pack-result` |
| `GET /<namespace>.git/info/refs?service=git-receive-pack` | write | `application/x-git-receive-pack-advertisement` |
| `POST /<namespace>.git/git-receive-pack` | write | `application/x-git-receive-pack-result` |

Write includes read; admin includes write. Protocol versions 0/1/2 use the same
access decision, before decoding controls or gzip/x-gzip bodies. POST media types
are the corresponding `application/x-git-*-pack-request`. The gateway must
preserve `Git-Protocol`, `Content-Type`, `Content-Encoding` and request bytes.
A valid read grant attempting receive-pack gets an HTTP 200 Git `ERR` packet
with “write access required; ask your gateway administrator”; discovery includes
the service announcement. This lets an ordinary terminal Git client explain
the refusal. It never reaches intake or publication. Non-Git privileged routes
require admin; insufficient access there returns HTTP 403 JSON.

Gateway mode never sends `WWW-Authenticate`. Missing/invalid/expired/wrong-scope
grants return HTTP 404 `{"error":"unknown route"}` with `Cache-Control: no-store`,
matching absent repository responses. No existence query occurs before grant
verification. The gateway decides how to challenge a client using its own
credentials. Git validation errors retain the existing HTTP 400 JSON shape
`{"error":"...","codecError":null}` (a numeric codec code may replace null);
storage failures use 500. Streamed protocol failures retain Git framing.

## Explicit lifecycle

All lifecycle routes use `/<namespace>.git/api/v1/repository` and require admin.
They are available over either transport, independent of `TEST_MODE`.

| Method | Operation | Success |
| --- | --- | --- |
| PUT | Create from an application/json object | 201 new, 200 identical existing settings |
| GET | Describe an existing repository | 200 |
| DELETE | Start or resume irreversible purge | 202 pending, 200 complete |

Create accepts only `objectFormat` (`sha1`, default, or `sha256`) and
`defaultBranch` (default `main`, a valid Git branch name up to 240 UTF-8 bytes).
Repeat the same settings for idempotence; conflicting creation settings return
409. The chosen symbolic HEAD stays on this branch even when it is unborn or
deleted; pushing a different branch does not change an explicitly chosen HEAD.
Existing implicit repositories retain their original HEAD-selection behavior.

Describe returns `namespace`, `state`, `objectFormat`, `head` (`symbolic` and
nullable `oid`), `defaultBranch`, `generation`, `objectCount`, `storedPackBytes`,
`storedBytes`, `databaseBytes` and `lastPublication`. Counts come from the catalog; storedPackBytes
is the sum of published encoded pack sizes, with no deduplication credit. `storedBytes` is the conservative size of unique catalog-owned R2 payload
keys (including temporary/quarantine data), once storage accounting is ready;
it is null while a legacy catalog is still being indexed. storageAccounting
reports ready and knownBytes during that bounded migration. Lost acknowledgments
can conservatively overcount until recovery. See policy below for exclusions.
`databaseBytes` reports the native SQLite size when the runtime exposes it. Times
are Unix milliseconds. `lastPublication` and the original default branch of a
legacy repository can be null when unknown; the next publication records its
time atomically with refs. The existing schema is preserved; `host_settings`
is an additive, idempotently created table.

Gateway mode **always requires explicit PUT creation**, including admin Git
requests. The transitional admin bootstrap has been removed. In token mode,
implicit creation remains the default; `IMPLICIT_CREATE=false` disables it.
An absent repository on a Git route returns the hidden 404. There is no global
namespace enumeration: the gateway owns its product repository catalog and
namespaces, rather than learning other tenants' identities from the host.

### Purge and retirement

DELETE first persists a deletion barrier and writes a permanent retirement
fence at `repository-tombstones/v1/<namespace>` in the same R2 bucket, outside
the repository content prefix. It then lists at most 64 objects under
`repositories/<namespace>/` and deletes that batch. Repeat DELETE while the
response is 202 (`state: deleting`, `complete: false`, `deletedObjects`). There
is no cursor to lose: each retry lists the first remaining page. Partial delete
failures return 500 and can be retried without losing progress. No content is
readable or writable after retirement starts, including by an earlier admitted
queued push. The repository queue finishes its previous owner before deletion.

After a separate list confirms the prefix is empty, native Durable Object
`storage.deleteAll()` reclaims the SQLite database and KV state. Only a tiny
retirement tombstone is then saved. The external fence closes the crash window
between native clearing and saving that tombstone. A cold instance checks the
fence before opening repository state; an interrupted purge resumes even when
its SQLite state or local tombstone has gone away. Alarms do not run ordinary
recovery against a retired repository. All content objects, including orphaned
quarantine/temporary objects under the prefix, are reclaimed. Objects in other
namespace prefixes are untouched. The fence is intentionally retained.

A retired namespace is never reusable: PUT returns 410, Git requests return 404,
and repeated DELETE returns 200 once complete. GET during purge returns 202;
a completed retirement returns 410 with its state. To reuse a product-level
name, the gateway assigns a fresh opaque namespace, such as a new UUID.
Each purge call has one bounded list, at most 64 object deletions, and at most
one native database-clear operation. No repository-sized JS array or SQL row
walk is used. Direct workerd's S3 adapter uses signed, bounded ListObjectsV2
responses; Wrangler uses the native R2 binding. One additional R2 fence lookup
is required on a cold instance. Diagnostics count it separately as
`r2FenceGets`, preserving `r2Gets` for data-chunk reads; benchmark reports retain
both counters. S3 fixture key-index sidecars are mock metadata, not payload.
No deployment cost is inferred from this.

## Read API

GET `/<namespace>.git/api/v1/{refs,history,tree,blob,commit}` requires read.
Write and admin include read. These operations never create a repository.
JSON responses use application/json and no-store; errors are
`{"error":"description"}` (codec failures can add `codecError`). Missing or
unreachable objects/paths return 404, ambiguous revisions or stale cursors 409,
wrong object kinds and submodule descent 422, malformed parameters 400, and
exhausted bounds 413. Storage failures remain 500.

`rev` defaults to HEAD. It accepts a full object ID, full ref name, or unambiguous
short branch/tag name; annotated tags are peeled. Use `refs/heads/name` to avoid
short-name ambiguity, or `oid:<full-id>` to disambiguate an ID-shaped ref name.
IDs have 40 hex digits in SHA-1 repositories and 64 in SHA-256 repositories.
Returned IDs are lowercase, including links parsed from valid uppercase commit headers.
Paths are relative, slash-separated UTF-8; empty means the revision's root.
Empty components, dot, dot-dot and NUL are refused. Symlinks are returned as
blobs and never followed; a submodule identifies content in another repository.

All selectors are restricted to the closure of the **current refs**. A read
rooted at HEAD, a branch or a tag follows already authenticated graph links;
its cost depends on the requested history page and visited directories/objects,
not the repository's total history. It does no reachability-index row writes.
Git's existing whole-catalog fetch semantics are unchanged.

Raw object IDs require a separate persisted proof index. Each request advances
at most `readLimits.indexSteps` graph work units (default 256; an edge or node
costs a unit, plus bounded bookkeeping). Proofs are reusable for the entire ref
generation. Current-generation positive proofs may be used before completion.
Until absence is proven, an unproven published ID returns 503,
`codecError: "reachability_index_building"`, `Retry-After: 1`, and a message
saying the index is being built. Retry the same request to advance it. After
completion an unreachable ID returns 404. Unpublished, rejected and quarantined
IDs return 404 immediately. Publication invalidates old proofs and signed
browse cursors; stale proof rows are reclaimed in bounded batches. Defaults
permit one million objects and ten million edges. At a configured index cap,
503 explains which budgets to raise; ref-rooted reads continue to work. Neither
selector shells out to Git or materializes a repository in memory. The
persistent index grows with reachable object count.

| Route | Parameters | Result |
| --- | --- | --- |
| refs | limit, cursor | generation, head, refs, nextCursor |
| history | rev, limit, cursor | generation, first-parent commits, nextCursor |
| tree | rev, path, limit, cursor | generation, directory oid, entries, nextCursor |
| commit | rev | one commit's metadata (tags peeled) |
| blob | rev + path, **or** oid; maxBytes | streamed exact blob bytes |

`limit` defaults to 20 and is at most 100 by default. Opaque signed v2 cursors include the
operation, namespace and ref generation. Pass them unchanged; subsequent
history/tree pages may omit rev/path. Changing those selectors is refused.
After any ref publication, restart pagination on 409. Responses can contain
fewer entries than limit to fit the byte budget. Cursors are not authorization:
each request requires a valid grant. HMAC-SHA-256 binds continuation proofs to
the operation, namespace and generation using a domain-separated deployment
authentication key. Tampered cursors fail with 400; key rotation invalidates them.

Refs include branches, tags and other refs with name, name64, kind, oid and
nullable peeled ID. HEAD includes symbolic, symbolic64 and nullable oid. Names
use a nullable strict UTF-8 string plus exact unpadded base64url bytes (name64),
so non-UTF-8 Git names are preserved. Empty repositories return empty refs and
history; other absent HEAD reads return 404. An explicit missing revision also
returns 404 in an empty repository.

Commit entries have oid, tree, parents, author, committer and message, plus an
optional encoding label. Identities have name/email (UTF-8 replacement decoding),
exact name64/email64, time as a decimal Unix-seconds **string** and timezone as
Git's signed four-digit offset. Strings preserve Git's wide integer timestamps.
Message has subject (first paragraph joined with spaces), body, raw64 and
truncated. raw64 is the exact prefix; text is UTF-8 replacement decoding, with
no untrusted HTML interpretation. The prefix is at most 32 KiB, commit headers
128 KiB, identities 4 KiB and parent count 256. Oversized headers return 413;
large messages are explicitly truncated. History follows first parents, even
when commits have multiple parents.

Tree entries have name/name64, six-digit canonical Git octal mode, rawMode
(the stored numeric octal permissions, preserving historical aliases), kind (blob, tree, symlink,
submodule), oid and nullable size. Blob sizes come from the verified catalog,
without reading their bodies. Each visited directory is at most 1 MiB; paths
are at most 4096 UTF-8 bytes and 64 components. Tag peeling is bounded to 32.
Metadata reads total at most 8 MiB per request and JSON at most 512 KiB. Codec
read/reconstruction work is at most 128 MiB and retains the hard object limits.
Whole authenticated chunk cache misses have a separate 128 MiB storage-byte
budget (readLimits.storageBytes), even when a lowered cache cannot retain a
legacy extent; bounded storage retries can attempt each miss at most three times.
A directory can be rescanned on each page; pagination bounds output and does
not imply unbounded scan work.

Blobs default to an 8 MiB maximum. maxBytes can lower that bound; exceeding it
returns 413 before any bytes. Responses carry Content-Length and X-Object-Id,
Content-Type: application/octet-stream, Content-Disposition: attachment,
X-Content-Type-Options: nosniff, and Cache-Control: no-store. A fixed-length
stream makes the length visible through workerd without buffering the file.
Serve this response unchanged: do not infer an executable browser media type
from repository filenames. Range reconstruction streams in codec pages,
including delta bases; it does not warm or materialize entire canonical bases.
No archive route is implemented; the optional archive was omitted to keep this
bounded surface small.

## Durable push record

GET `/<namespace>.git/api/v1/changes` requires read. Every successful publication
records generation, Unix-millisecond time, the verified grant's opaque actor,
and every accepted **changed** ref in its original command order. Updates carry
seq, name/name64, oldId and newId; creations/deletions use the format's all-zero
ID for the absent side. No-op updates and rejected refs are omitted. A partially
accepted non-atomic push records its accepted changes; a rejected atomic push
has no publication. Ref-only updates are recorded even when their redundant
private pack is subsequently reclaimed. Token mode records an empty actor.

The record, ref updates, generation and lastPublication time commit in one SQLite
transaction. A journal write failure rolls back publication. Actors are stored
as canonical base64url of exact authenticated UTF-8, avoiding runtime SQL text
truncation at NUL; returned labels preserve empty strings, NUL, BOM and whitespace
without interpretation. Publication rows remain until repository deletion; there
is no retention/pruning policy or outbound notification. Generation order is
authoritative; wall-clock times need not be strictly monotonic.

Parameters: after (completed generation, default 0), limit (maximum **update
rows**, default 20, maximum 100), cursor. Responses are at most 512 KiB and have:

```json
{
  "recordedAfterGeneration": 0,
  "throughGeneration": 1,
  "completedThrough": 0,
  "publications": [{
    "generation": 1, "time": 1700000000000, "actor": "opaque label",
    "updates": [{"seq": 0, "name": "refs/heads/main", "name64": "cmVmcy9oZWFkcy9tYWlu", "oldId": "...", "newId": "..."}],
    "complete": false
  }],
  "nextCursor": "opaque-base64url"
}
```

A publication can span multiple pages. Accumulate its updates until complete is
true; advance the durable completed generation only through completedThrough.
While partial, persist nextCursor and any partial processing state together.
Retrying a page is safe; consumers should deduplicate by (generation, seq).
When nextCursor is null, resume later with after=completedThrough. The cursor
snapshots throughGeneration and remains valid when later pushes arrive; unlike
browse cursors it does **not** expire on ref publication. A new after request
then catches those newer pushes. Cursors are namespace/operation-bound but are
not credentials; every request still needs a valid read grant. after beyond the
current generation returns 409; malformed/contradictory parameters return 400.

On upgrading an existing repository, recordedAfterGeneration is its generation
before the journal became available. Earlier events/actors cannot be recovered.
An after below that boundary returns 409 with recordedAfterGeneration; reconcile
the repository snapshot, then begin at that boundary. New repositories start at
zero. Existing SQLite tables are unchanged: publication tables and the coverage
boundary are additive and initialized idempotently. The log contains historical
ref names, IDs and actors even if those objects are no longer browse-readable.
The gateway can restrict its public product routes further if needed.

## Repository policy and storage bounds

Admin GET/PATCH `/<namespace>.git/api/v1/policy` reads/changes policy for an
existing repository. PATCH is an application/json object containing any of:
allowNonFastForward (boolean), maxPushBytes and maxStoredBytes (nonnegative safe
JSON integers). Null removes an override. Unknown fields/types return 400.
GET/PATCH responses have policy (effective values), overrides (nullable),
deployment (defaults/ceilings), and storage (ready, knownBytes). Policy changes
do not change the ref generation. Read/write grants cannot change or read policy.

The reference deployment uses ALLOW_NON_FAST_FORWARD (`true`/`false`, default
false), MAX_PUSH_BYTES and MAX_STORED_BYTES (canonical decimal, absent/empty means
uncapped). Per-repository force permission overrides its deployment default.
Effective size caps are the smaller of repository and deployment caps: admin
cannot raise a deployment's ceiling. Existing codec/protocol quotas remain hard
outer limits even when these optional caps are absent or higher.

Force permission skips branch ancestry rejection only. Current old-ID
compare-and-swap, target type, full pack/graph verification, ref-name conflicts
and atomic failure semantics still apply. The default continues to reject
non-fast-forward branch moves. Tag updates keep their existing semantics.

maxPushBytes counts the **decoded Git pack**, including its pack header and
trailer, excluding independently bounded command controls and transport framing.
Gzip/x-gzip cannot evade it. A deletion-only request has zero pack bytes. A pack
that exceeds the cap is refused before any excess bytes are persisted; accepted
controls allow an unpack/ref status with “single-push size limit exceeded”.
Stored-size rejection similarly reports “repository stored-size limit exceeded”
to an ordinary pushing Git client. Rejection leaves refs/generation/log unchanged
and reclaims the private stage, or retains its claims for scheduled recovery if
storage cleanup fails.

maxStoredBytes bounds **unique, catalog-owned R2 payload bytes** under the
repository content prefix. It includes published/unreachable retained chunks,
quarantine and **push-validation** temporary reconstruction chunks. Shared content-addressed keys
count once. This is an object-storage cap: SQLite pages/indexes/journal rows,
R2 metadata overhead and the permanent retirement fence are excluded. Native
SQLite bytes remain separately reported where available and subject to platform
limits. External/manual bucket writes are not host-owned claims. This explicit
payload definition avoids pretending to know physical R2 billing or SQL overhead.

A durable per-key ledger and byte counter reserve space in the same transaction
as chunk ownership, before R2 PUT. Failed/lost PUT acknowledgments can overcount;
that is conservative. Space is released only after an acknowledged R2 deletion.
Recovery retries lost acknowledgments without double charging. Lowering a cap
below existing usage does not delete content: browse reads, deletion-only pushes and
updates needing only already owned keys remain possible; new keys are refused.
Read-only fetch reconstruction uses separately owned **read scratch**, excluded
from stored-size accounting and its readiness gate. A clone/fetch can therefore
serve existing content when usage exceeds the cap or legacy accounting is
pending. Scratch markers and temporary ownership are committed together before
PUT and retained until deletion is acknowledged; cold recovery reclaims them.
Accounting backfill excludes these markers, including deferred-cleanup scratch.
Push reconstruction still needs quota headroom. Whole repository DELETE purges
all content, including scratch and objects outside the known ledger.

Fetch preflights exact output pack size and every fallback reconstruction before
sending 200. Small fallback bodies are pinned in owned scratch too, so cache
eviction cannot trigger reconstruction/quota errors mid-stream. Codec canonical,
work and temporary budgets remain enforced; a known preparation failure returns
an HTTP error before response bytes and leaves refs unchanged. This moves work
and temporary R2 writes earlier, increasing time to first byte for fallback
fetches; reusable compressed packs keep their existing streaming path. Storage
or transport failures after preparation remain unpredictable stream failures.

New/empty catalogs have ready accounting immediately. Existing catalogs keep
working uncapped without a table rewrite. Their old stage/temporary ownership
rows are indexed **64 at a time**, with a durable composite cursor and deduped
physical keys. Admin POST `api/v1/policy` (no body) advances one page and returns
202 until storage.ready is true, then 200. It is idempotent after completion.
A capped push advances one page before writing; if accounting is still pending,
it reports “stored-size accounting pending; administrator must resume POST
api/v1/policy” and cannot bypass the cap. New writes/deletes update the ledger
during migration, so concurrent requests/restarts cannot undercount a ready
catalog. No repository-sized JS array or unbounded migration scan is required.

## Composing the host in another Worker

Import `createRepositoryHost` from
`examples/repository-host/cloudflare/src/host.mjs`. Its only public export is the
factory. It returns `{ fetch, Repository }`: a Worker-compatible request handler
and a Durable Object class to re-export under **your** chosen name. It imports
no Wasm asset and assumes no external binding or route. `streamed.mjs` is the
thin reference deployment, selecting its historical names, modes and routes.
The same factory supports embedded and separate Worker deployments.

```js
import codec from "./assets/git_zero_host_wasm.wasm";
import { createRepositoryHost } from "./vendor/git-zero/cloudflare/src/host.mjs";
import productWorker from "./pkg/product_worker.js";

const repositories = createRepositoryHost({
  codec,                         // compiled WebAssembly.Module, bundled by you
  bindings: {
    repositories: "PRODUCT_REPOS",
    objects: "PRODUCT_BUCKET",   // can be your existing shared R2 bucket
    grantKey: "PRODUCT_GRANT_KEY" // secret binding, never a public variable
  },
  authMode: "gateway",
  routePrefix: "/internal/git",
  keyPrefix: "product/git/",
  memory: {
    canonicalCacheBytes: 2 * 1024 * 1024,
    representationCacheBytes: 1024 * 1024,
    chunkCacheBytes: 2 * 1024 * 1024,
    cacheEntries: 128,
    canonicalCaptureBytes: 1024 * 1024,
    representationCaptureBytes: 256 * 1024,
    chunkBytes: 65536,
    refPageRows: 16
  },
  maxRequestsPerRepository: 16
  // healthPath/testPrefix omitted: no library health or diagnostic routes
});
export const ProductRepository = repositories.Repository;
export default { fetch: (request, env, ctx) => productWorker.fetch(request, env, ctx) };
```

Your Wrangler configuration binds PRODUCT_REPOS to the re-exported
ProductRepository class, declares it in new_sqlite_classes in a migration, binds
PRODUCT_BUCKET to your existing R2 bucket and supplies PRODUCT_GRANT_KEY as a
secret. Each repository gets a **dedicated** instance of this class/database;
its irreversible deleteAll must not share a Durable Object database with
unrelated product state. The surrounding Worker script/isolate and R2 bucket
can be shared. A workers-rs router calls the namespace binding directly using
the contract below; the JS wrapper need not call repositories.fetch at all.
The fixture uses a tiny JS router as the Rust caller stand-in, not a workers-rs
integration/build test.

For a separate Worker, export `{ fetch: repositories.fetch }` as its default and
re-export the class exactly the same way. A gateway invokes that fetch handler
via a service binding or plain HTTP with the configured route prefix and grant.
The outer handler overwrites the namespace header from the URL. It does not
provide privileged context unavailable to the Durable Object.

### Direct Durable Object request contract

1. Map your product name to its opaque namespace. Obtain the stub using the
   **configured repository binding's idFromName(namespace)** (the bare namespace,
   without `.git`, prefix, team name or any other transformation).
2. Send the stub a Request with the original method and this URL path:
   `<routePrefix>/<namespace>.git/<Git-or-api/v1-route>`. The URL authority is
   arbitrary; use a syntactically valid absolute URL, for example
   `https://repository.internal/internal/git/<namespace>.git/api/v1/refs`.
3. Supply `X-Git-Zero-Namespace: <namespace>` and `X-Git-Zero-Grant: <fresh grant>`.
   The header, URL namespace, grant namespace and the stub's binding-derived
   object ID must agree. An absent/mismatched value returns the hidden 404.
   Token mode instead forwards its configured Bearer/Basic credential; the
   namespace header is still required. No trusted-binding shortcut exists.
4. Preserve Git-Protocol, Content-Type, Content-Encoding, query selectors and
   **body bytes/stream**. Use the Git POST media types in the route table. JSON
   create/PATCH requests use application/json; read requests use GET. Return the
   host response and its safe content headers unchanged to the client.

The Durable Object independently normalizes its configured path and verifies
access immediately in fetch, before the repository queue **or** isolate job
queue. No entry-only marker, prior initialization call, secret forwarded header,
or preverified access object is needed. It checks stored namespace/prefix
identity before recovery or retirement lookup. Calling the wrong stub with an
otherwise valid grant cannot poison either repository's state. Recovery and
alarm work run after authorization in the same bounded scheduling scheme.

### Factory options

Required: codec, bindings.repositories, bindings.objects, authMode, routePrefix,
keyPrefix. For a custom storage adapter, bucket(env) can supply the object store
instead of bindings.objects; it must implement native-R2-compatible get/put/list/
delete semantics (including bounded prefix listing). The reference uses that
seam for its local signed-S3/workerd adapter. Token/gateway auth reads only the
explicit bindings.token/bindings.grantKey respectively. Missing secrets fail
closed; unknown modes fail closed.

routePrefix is an explicit empty string or a slash-prefixed sequence of ASCII
letters/digits/underscore/dash segments, with no trailing slash. keyPrefix is an
explicit empty string or a slash-terminated sequence of ASCII letters/digits/
underscore/dash/slash (no double slash), at most 240 characters. Namespaces
remain single segments. Every content/tmp key and retirement fence uses that
prefix. Only `<keyPrefix>repositories/<namespace>/` is purged; neighboring
product keys, other repository prefixes and retirement fences survive.

Preserve the bucket, binding's Durable Object namespace and key prefix for a
stored deployment. An additive host_storage_config row records the prefix;
changing it on an existing object returns 409 rather than reading/deleting a
new location. Legacy catalogs require their original empty prefix. No automatic
bucket/prefix migration is implemented. A new logical host prefix/binding is a
different deployment identity and should use its own grant key.

Optional healthPath and testPrefix default to disabled. When explicitly set,
each is an absolute path with the same safe-segment rules. testPrefix must not
overlap routePrefix. testEnabled defaults to true when testPrefix is supplied;
it can be a trusted env-to-boolean function (the reference gates it on TEST_MODE).
Diagnostics require admin and must be enabled only for local/test deployments.
They are never enabled merely because an embedding Worker has TEST_MODE set.
healthPath is handled only by the Worker fetch entry, not by a repository stub.
It reports authentication readiness, not an end-to-end storage probe.

implicitCreate defaults to true for token mode; gateway mode always requires
explicit PUT. objectFormat is 0 (SHA-1, default) or 1 (SHA-256) for new implicit
repositories. authMode, implicitCreate, objectFormat, testEnabled and policy may
be trusted functions of env for deployments with configuration variables. These
selectors must be deterministic for a deployment's env. policy supplies
allowNonFastForward/maxStoredBytes/maxPushBytes as described above. The factory
has no knowledge of the reference variable names.

memory, codecLimits and readLimits accept known integer fields only. memory
and codecLimits may lower their exported defaults. readLimits may be raised or
lowered within the exported READ_HARD_LIMITS table. Unknown fields, unsafe
integers and values beyond the respective ceilings throw at construction.
Protocol capabilities and ABI frame sizes are not configurable. Source/cache
payload controls are listed below. codecLimits correspond to resource fields in
src/codec.mjs CODEC_LIMITS, including object/pack/control counts/bytes, ancestry,
canonical/work/temporary quotas; the same lowered limits apply to Git and browse
operations. readLimits correspond to READ_LIMITS in src/browse.mjs; pageSize and
jsonBytes also bound push-log pages. Lowering pageSize lowers its default too.
Some extremely small bounds deliberately make an operation return 413.

Read hard ceilings (defaults remain those above): 100 million proof objects,
one billion proof edges, 8192 index work units/request, ten million refs,
1000 rows/page, 16 MiB/tree, 1 MiB each commit headers/message, 32 MiB total
metadata, 64 MiB/blob, 65536 path bytes, 1024 path components, 256 tag depth,
4096 parents, 65536 identity bytes, 8 MiB JSON, and 1 GiB each reconstruction
work/storage reads. Lower codec limits remain the hard outer limits. Raising
metadata/output limits increases memory and CPU use. Ref-rooted reads remain
available as history grows; individual oversized objects, directories or pages
can still exceed request budgets and return 413.

The generated `browse-scale.test.mjs` publishes 25,000 commits / 100,003 objects,
several refs and a branch rewrite in both Git object formats. Against its
1,000-commit / 4,003-object comparison, statement counts are unchanged: HEAD
commit 10, first-parent history page of 20 commits 47, nested tree 15, nested
blob 15; all four perform **zero row writes**. These counts include admission
and lifecycle checks in the focused SQLite adapter. They are test-fixture
measurements, not Cloudflare billing estimates. Real Git supplies object, tree,
blob and history oracles; raw-ID proof progress and reuse are tested separately.

### Memory and shared-isolate scheduling

The following are payload/element bounds **per active job**, not a measurement
of the complete JavaScript VM. Reserve space for the embedding Rust Wasm, its
allocator, HTTP/runtime buffers, SQL result objects, V8 metadata and garbage
collection inside the isolate's 128 MB limit. Concurrent repository jobs
have independent envelopes which add together. Inactive repository instances retain
Store/lifecycle/configuration metadata, not byte caches or codec instances. A
compiled module and its engine code metadata are shared per factory, outside
linear-memory totals. The runtime controls how many Durable Object instances
stay resident; the host does not impose an isolate-wide instance/job cap. Per-instance metadata
and SQLite/runtime handles must also be budgeted. memory.metadataEntries lowers
the stage-alias cache retained by each Store.

| Component | Token / gateway default bound | Configuration (ceiling: token default) |
| --- | --- | --- |
| Rust codec instance, including control arena and graph/name scratch | fixed 6 MiB linear memory, one per Git/body-read job | ABI fixed; reduce concurrent jobs |
| Canonical byte cache | 24 / 2 MiB | memory.canonicalCacheBytes |
| Representation byte cache | 8 / 1 MiB | memory.representationCacheBytes |
| Authenticated chunk byte cache | 16 / 2 MiB | memory.chunkCacheBytes |
| Entries per each byte cache (including empty values) | 8192 / 1024 | memory.cacheEntries; zero disables caching |
| One canonical reconstruction capture | 8 / 1 MiB | memory.canonicalCaptureBytes; larger objects use bounded temp R2 |
| Intake/current representation capture | 1 MiB / 256 KiB | memory.representationCaptureBytes |
| Each active encoded/temp chunk writer | 1 MiB | memory.chunkBytes, minimum 64 KiB |
| Git ref SQL page | 128 / 16 raw names, each bounded by Git control framing | memory.refPageRows, minimum 1; lower codecLimits.maxControlBytes/maxCommands too |
| Codec input/output frames and source pin | 64 KiB / 128 KiB / 64 KiB | fixed ABI pages |
| R2 chunk miss, outside cache while authenticating | up to 8 MiB for legacy chunks; new chunks up to chunkBytes | historical extents remain readable; runtime digest/copy overhead also applies |
| Delta/base aliases, temporary descriptors and Store stage aliases | 256 / 128 entries each; range depth default 128 | memory.metadataEntries (zero disables alias caching), codecLimits.maxDepth |
| Commit/tree metadata prefixes | total 8 MiB per browse request, plus text/base64/JSON copies | readLimits.metadataBytes/headerBytes/messageBytes/treeBytes |
| JSON page | 512 KiB; up to 100 rows plus one lookahead | readLimits.jsonBytes/pageSize; string escaping/base64 use additional transient buffers |
| Publication command/outcome/peel metadata | at most 16384 commands in 4 MiB incoming controls | codecLimits.maxCommands/maxControlBytes; SQL/V8 per-row overhead is additional |

Caches have independent byte and entry caps. Captures are additional to caches;
lower both if reducing the envelope. Owned intake buffers can be shared by two
caches and are released when their last owner retires them. Authentication and
metadata-only lifecycle/refs/log requests allocate no codec. Delta streaming uses
range queries, not whole-repository buffers. Ordinary SQL work pages are at most
128 rows (cleanup ownership pages at most 1000 small records); graph frontiers
live in SQLite. Configurable ref pages bound the large raw-name case. A chunk
miss can transiently coexist with cached chunks and runtime cryptographic copies.
A lower new-chunk size does not shrink an existing 8 MiB legacy extent.

There is **no isolate-wide semaphore or admission queue**. Each repository
serializes its own mutable scratch, codec and response lifetime. A stalled
reader of A holds only A's active slot, never B's admission or queue. A caller
filling A and B cannot cause a host-generated busy response for C. Every
repository's queue is FIFO, including alarms/recovery and deletion. Authorization
is captured before that queue. An admitted grant may expire while queued.

Gateway mode permits 32 outstanding requests **per repository**, including the
active one. Excess requests to that repository return 503 and Retry-After: 1.
`maxRequestsPerRepository` can set an explicit 1..65536 bound in either mode.
Token mode has **no new admission bound by default**, retaining its earlier
queue/load behavior. Removed experimental maxActiveJobs/maxPendingJobs options
are rejected at construction so a stale configuration cannot silently change.

Gateway streams have a 10-second client-idle deadline and a five-minute total
lifetime. `responseIdleMs` accepts 1..600000 ms; `responseTotalMs` accepts
0..3600000 ms (zero disables the total deadline). The legacy token defaults
remain 60 seconds idle and no total deadline. Active consumers reset only the
idle deadline. Timeout/cancellation aborts the stream and releases scratch after
generator cleanup; an outstanding storage operation must settle before cleanup
can finish. Slow uploads have the independent request-body idle bounds. Bodies
are not intentionally buffered while queued; platform transport buffers exist.
DELETE waits behind an active fetch until completion/cancellation/timeout.

Gateway defaults reduce the three byte caches from 48 MiB to 5 MiB per job,
canonical/representation captures to 1 MiB/256 KiB, and ref pages to 16 rows.
Explicit memory values may increase these to the token ceilings or lower them.
This avoids a cross-tenant admission bottleneck; it is not a total-isolate RSS
reservation or a guarantee against exhaustion. Multiple active namespaces add
codec, cache, object metadata, SQLite and runtime overhead inside 128 MB. Shared
CPU/event-loop scheduling, platform connection limits and finite memory can
still affect co-tenants; bound public load and measure the surrounding product.
There is no measured production RSS or billing claim. After streamed headers,
unpredictable I/O failures abort the stream; fixed Content-Length detects a
partial blob. Known read-resource failures are checked before headers.

### Building and bundling the codec

Pin the full source commit and Cargo.lock, the compiler and target, and the
hosting profile. The codec sources/ABI used here are unchanged from this tested
pin: `9efd2f70da9247411feb6bd79d4b291cd618c6b6`. That pin is an existing ancestor of main, so squash merging this PR does not
remove it. `cloudflare/codec-source-pin.json` records its build-input digest;
the suite checks current codec sources and manifests against that digest.
In a build of that pinned source:

```sh
rustup toolchain install 1.99.1 --profile minimal --target wasm32-unknown-unknown
RUSTFLAGS='' CARGO_ENCODED_RUSTFLAGS='' cargo +1.99.1 build --locked \
  --profile hosting -p git-zero-host-wasm --target wasm32-unknown-unknown
sha256sum target/wasm32-unknown-unknown/hosting/git_zero_host_wasm.wasm
```

Copy that artifact to your own asset directory and import it as a compiled
WebAssembly.Module in your JS entry, passing it as codec. The library does not
compile bytes at runtime or know this repository's build/ path. Vendor the host
JS and matching codec ABI together; keep the compiler/lock/profile pin in your
product's build manifest and verify its artifact digest. This workflow adds no
runtime npm or no_std dependency. Local independent build roots with Rust
1.99.1 with the explicit empty flag environment above produced byte-identical
artifacts, SHA-256
`3b7ad4b3646070a22534481a9ce93aa9f5fb047a02887fddcbfcc15953617c5a`.
The independently built artifact is also tested against the codec and product
APIs. Other compiler/flag selections can change that digest; no prebuilt release
is downloaded.

## Scope and verification

Access levels, grants, explicit lifecycle and the reachability-restricted read
API, durable actor/push records, policy and configurable composition are
implemented for embedded and separate Workers. LFS, SSH,
application hooks, unreachable-object garbage collection, and a native
push-capable host are not implemented.

The local `test/fixtures/gateway.mjs` Worker authenticates a tiny in-memory
Basic user table, maps `alpha/analysis` to a base64url namespace, mints grants,
and forwards through a service binding. Its keys/passwords are public test data;
it is not a production gateway. `test/gateway.test.mjs` drives push, clone,
fetch, strict fsck, denied reader pushes and hidden outsider responses with
real Git in v0/v1/v2, including gzip controls. It repeats grant use against
plain HTTP `wrangler dev`. Existing token-mode tests remain unchanged in meaning.
No deployed latency, billing cost or production Cloudflare behavior is claimed.

Browse fixtures are generated by real Git in both object formats and compared
with for-each-ref, ls-tree, cat-file, log/show and rev-list. Both local runtimes
accept real Git pushes and serve the same metadata and bytes. Negative coverage
includes unreachable retained objects after ref deletion/recreation, stale
cursors, ambiguity, raw names, large trees, truncated messages and work limits.

The embedded fixture re-exports ProductRepository under renamed bindings,
uses a product-owned health route, and normally forwards directly to its stub.
Real Git in both formats exercises push/clone/fetch, read/write denial, safe
blob reads and journal actors. It writes foreign objects adjacent to the host
prefix, purges the repository and proves those objects survive, including cold
restart retirement. A forced wrong-stub request cannot poison another namespace.
A separate module test holds one repository's response beyond a second
repository grant's expiry: per-repository admission succeeds, expired arrivals remain
404 and excess valid arrivals receive 503. Lower page/cache/capture/chunk bounds
are exercised; the reference gateway/HTTP/S3 runtimes remain independently tested.

### Storage configuration and jurisdiction safety

Every Store construction, including alarm recovery, checks prefix identity before
creating schema or recording configuration. Legacy catalogs require the empty
prefix on every path; an alarm cannot silently adopt a new prefix.

The optional factory jurisdiction is null (unrestricted namespace) or `eu`.
Both entry routing and the Durable Object identity check call the configured
binding's jurisdiction("eu") before idFromName. Direct callers must use the
same jurisdiction. A global/different object ID still returns the hidden 404.
Jurisdiction is deployment identity: preserve it with the namespace/bucket/prefix;
R2 location is configured separately by the embedder. Local workerd does not
implement jurisdiction placement. Tests use a strict scoped-binding adapter with
distinct native IDs and verify mismatch rejection; no live EU placement is claimed.

Boolean configuration accepts only true/"true" as enabled, false/"false" as
disabled. Other supplied values (including null/undefined from a selector) fail
closed for testEnabled and implicitCreate. Omitted token implicitCreate retains
its compatibility default. Gateway creation always requires explicit PUT.
Authorized production reads of a nonexistent repository do not create tables,
identity or KV records. Invalid creation is validated first. Old schema-only
identities left by earlier versions can be purged with admin DELETE even when
no repository row exists.

The `fetch-quota.test.mjs` regression drives real Git against 9 MiB delta blobs
in both formats with a zero stored-size cap, then reconstructs a cold catalog
with pending legacy accounting and fetches again. Both complete and verify the
Git object ID/size. A lowered temporary budget is refused before 200. Existing
native workerd tests retain their EOF and deferred scratch recovery assertions.
