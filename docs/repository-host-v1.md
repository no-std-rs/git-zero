# Repository host HTTP contract v1

This contract describes implemented routes in `examples/repository-host/`.
A gateway owns users, credentials, teams and repository-name mapping. The host
owns Git and trusts only cryptographically verified repository grants. HTTP and
Cloudflare service bindings have identical authentication semantics. Use TLS
outside local loopback. Do not forward grants to clients or log them.

## Configuration and identity

`AUTH_MODE=token` (also the unset default) preserves the existing `HOST_TOKEN`
secret: Bearer credentials or the password in HTTP Basic grant full access.
Invalid credentials receive HTTP 401 with the existing Basic challenge.
`AUTH_MODE=gateway` ignores `HOST_TOKEN` and requires the `GRANT_KEY` secret,
a UTF-8 string of at least 32 bytes. Generate a high-entropy key, unique to this
host deployment. Unknown modes fail closed. `/health` reports readiness without
exposing configuration values. Store actual keys in deployment secrets, never
in a committed Wrangler `[vars]` section.

Namespaces are opaque single path segments matching `[A-Za-z0-9_-]{1,80}`;
UUIDs are accepted. A gateway maps its own `team/repository` identity to one of
these namespaces (an injective encoding or a stored identifier). It must strip
client-supplied grants and the internal `x-git-zero-namespace` header before
forwarding, and replace the grant with one minted from its own authorization
decision. Namespace identity comes from the URL; the Worker overwrites the
internal header and the Durable Object checks both again.

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
Paths are relative, slash-separated UTF-8; empty means the revision's root.
Empty components, dot, dot-dot and NUL are refused. Symlinks are returned as
blobs and never followed; a submodule identifies content in another repository.

Every object lookup, including a raw ID, is restricted to the closure of the
**current refs**. Typed authenticated edges are traversed in bounded SQLite
pages. Unreachable bytes retained from earlier generations cannot be read via
this API. Git's existing whole-catalog fetch semantics are unchanged. The
reachability walk has 20,000 object, 100,000 edge and 20,000 ref defaults and
fails closed with 413 on exhaustion. It does not materialize a whole repository
or shell out to Git. These limits also bound work on disconnected histories.

| Route | Parameters | Result |
| --- | --- | --- |
| refs | limit, cursor | generation, head, refs, nextCursor |
| history | rev, limit, cursor | generation, first-parent commits, nextCursor |
| tree | rev, path, limit, cursor | generation, directory oid, entries, nextCursor |
| commit | rev | one commit's metadata (tags peeled) |
| blob | rev + path, **or** oid; maxBytes | streamed exact blob bytes |

`limit` defaults to 20 and is at most 100. Opaque base64url cursors include the
operation, namespace and ref generation. Pass them unchanged; subsequent
history/tree pages may omit rev/path. Changing those selectors is refused.
After any ref publication, restart pagination on 409. Responses can contain
fewer entries than limit to fit the byte budget. Cursors are not authorization:
each request is granted and each selected object must still be reachable.

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

Tree entries have name/name64, six-digit octal mode, kind (blob, tree, symlink,
submodule), oid and nullable size. Blob sizes come from the verified catalog,
without reading their bodies. Each visited directory is at most 1 MiB; paths
are at most 4096 UTF-8 bytes and 64 components. Tag peeling is bounded to 32.
Metadata reads total at most 8 MiB per request and JSON at most 512 KiB. Codec
read/reconstruction work is at most 128 MiB and retains the hard object limits.
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
quarantine and temporary reconstruction chunks. Shared content-addressed keys
count once. This is an object-storage cap: SQLite pages/indexes/journal rows,
R2 metadata overhead and the permanent retirement fence are excluded. Native
SQLite bytes remain separately reported where available and subject to platform
limits. External/manual bucket writes are not host-owned claims. This explicit
payload definition avoids pretending to know physical R2 billing or SQL overhead.

A durable per-key ledger and byte counter reserve space in the same transaction
as chunk ownership, before R2 PUT. Failed/lost PUT acknowledgments can overcount;
that is conservative. Space is released only after an acknowledged R2 deletion.
Recovery retries lost acknowledgments without double charging. Lowering a cap
below existing usage does not delete content: reads, deletion-only pushes and
updates needing only already owned keys remain possible; new keys are refused.
Temporary storage needs headroom inside the same cap. Whole repository DELETE
still purges the prefix, including objects outside the known ledger.

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

## Scope and verification

Access levels, grants, explicit lifecycle and the reachability-restricted read
API, durable actor/push records and repository policy are implemented. A
configurable host-module boundary remains the final milestone. LFS, SSH,
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
