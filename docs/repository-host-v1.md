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
again immediately on arrival at the Durable Object, before admission or cold
recovery. Verified access and actor travel with the job; admission, byte-budget
or cold-recovery waiting cannot expire an accepted request.
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
push. Deletion fences immediately, cancels all live jobs (including cached-pack
builders and maintenance), and returns 202 with waitingForJobs while their
storage I/O and generator cleanup settle. In-flight clones/blobs are aborted;
Git must retry against replacement content under a fresh namespace. Purge never
removes bytes while an earlier job may still read or write them. DELETE bypasses
ordinary admission so a saturated reader queue cannot prevent retirement.

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
Reconstruction cost also depends on the visited objects' delta-chain depths,
compressed chunk misses and directory widths; it is independent of unrelated
history. Bounds on path depth, returned rows, metadata, storage reads and codec
work still apply. Raising those bounds cannot make a single object unbounded.

Every owned object ID returned by refs/history/tree/commit has an opaque signed
proof. Pass it unchanged as `proof=` with a subsequent raw-ID request. Refs and
HEAD have `proof`, annotated refs also `peeledProof`; commits have `proof`,
`treeProof` and position-matched `parentProofs`; directory responses and their
entries have `proof`. A submodule's external ID has a null proof: it does not
identify an owned object. Authenticated child links from a proven object need
no global index. Thus history -> commit -> tree -> blob works immediately after
a push, including while a raw index is unfinished. Proofs never replace grants.

Proofs and all cursors use signed v3 payloads containing operation, namespace,
a persisted random repository **epoch**, and generation. The per-repository
HMAC-SHA-256 key is derived with HKDF-SHA-256 from the configured grant/token
key, with UTF-8 epoch as salt and UTF-8 JSON
`["git-zero-browse-v3", signingContext, keyPrefix, namespace]` as info.
`signingContext` defaults to an empty string; embedders can set a distinct stable
context per deployment (at most 240 characters), including for restored copies.
The payload and MAC use canonical unpadded base64url, separated by a dot; the
MAC input is `git-zero-browse-v3\n` plus the encoded payload. Consumers treat
them as opaque. Independent fresh repositories sharing a key/name/generation
cannot reuse tokens. Epoch backfill is additive and idempotent for legacy data.
Key rotation or changing signingContext invalidates outstanding tokens.

Object proofs and persisted positive membership survive **additive** publications:
creations and verified fast-forwards. Publication adds new tips in the ref
transaction and preserves previous membership, completed or unfinished work.
Non-fast-forward updates, tag replacements and any ref deletion change the proof
lineage: older object proofs fail with 409 and the index rebuilds from current
refs. Browse pagination cursors are invalidated by every publication. Push-log
cursors retain their snapshotted range across publications, including rewrites;
they are still epoch-bound and signed. Old v2 browse/v1 push-log cursors must be
restarted after upgrading.

Raw object IDs without an applicable proof use the persisted index. Pending work
walks **outward from current tips**, in increasing graph depth. Each request or
alarm advances at most `readLimits.indexSteps` units (default 8192, raisable to
65536; an edge or node costs a unit, with bounded bookkeeping). Publication
schedules a best-effort alarm; unfinished work schedules another bounded alarm.
Correctness rests on synchronous ref/proof transactions and request-driven
progress, not alarm delivery. Positive membership is usable before completion.
An unproven published ID returns 503, `codecError: "reachability_index_building"`,
`Retry-After: 1` and an explicit building message. After completion absence is
404. Unpublished, rejected and quarantined IDs return 404 immediately. Stale
rows are reclaimed in bounded pages. Defaults permit one million objects and
ten million edges; a paused index returns 503 explaining which budgets to raise.
Ref/proof-rooted browsing continues while it is paused. The index occupies
SQLite space proportional to known objects, never repository-sized JS buffers.

| Route | Parameters | Result |
| --- | --- | --- |
| refs | limit, cursor | generation, head, refs, nextCursor |
| history | rev, limit, cursor | generation, first-parent commits, nextCursor |
| tree | rev, path, limit, cursor | generation, directory oid, entries, nextCursor |
| commit | rev, proof | one commit's metadata (tags peeled) |
| blob | rev + path, **or** oid; proof, maxBytes | streamed exact blob bytes |

`limit` defaults to 20 and is at most 100 by default. Pass signed cursors
unchanged; history/tree continuations may omit rev/path. Changing selectors is
refused. Restart browse pagination on a stale-cursor 409. Responses can contain
fewer rows to fit the JSON byte budget. A valid grant is required on every
request. Invalid/tampered tokens return 400; a valid but revoked object proof
or browse cursor returns 409.

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
Canonical output reservation follows the configured metadata plus blob bounds
(up to their documented ceilings), rather than imposing a hidden 32 MiB cap.
Blob output/object quotas are reserved/checked before streaming headers; an
explicitly lower codec ceiling returns JSON 413. `browse-blob-budget.test.mjs`
streams a generated 33 MiB blob in each format with zero SQL row writes, and
verifies both lowered canonical/object ceilings fail before 200.
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

Gateway defaults are **256 MiB maxPushBytes** and **1 GiB codec maxPackBytes**
(the latter also bounds fetch output). Token mode retains its previous uncapped
policy and 64 GiB codec pack ceiling. Both gateway defaults may be raised
explicitly, up to the existing hard codec ceilings. At the default 16 KiB/s
transport floor, 256 MiB / 16384 is 16,384 seconds (**4.55 hours**) and 1 GiB /
16384 is 65,536 seconds (**18.20 hours**), plus initial credit and bounded
control/admission waits. These are transport-input/output time bounds; storage,
CPU and cleanup retain their separate progress/work/I/O limits. A raised 64 GiB
ceiling permits roughly **48.5 days** at that floor. Size limits and throughput
must be configured together; disabling the floor removes that time bound.

maxPushBytes counts the **decoded Git pack**, including its pack header and
trailer, excluding independently bounded command controls and transport framing.
Gzip/x-gzip cannot evade it. A deletion-only request has zero pack bytes. A pack
that exceeds the cap is refused before any excess bytes are persisted; accepted
controls allow an unpack/ref status with “single-push size limit exceeded”.
Stored-size rejection similarly reports “repository stored-size limit exceeded”
to an ordinary pushing Git client. On a policy failure during upload the host
stops parsing/hashing/persisting and drains the remaining **decoded** body to
EOF before returning Git's unpack/ref report. The default discard bound is
64 MiB after refusal, 30 seconds wall clock and at most 10 seconds idle. Early
hidden/authentication/read-only/busy/unknown-route POST replies instead discard
bounded **encoded** bytes without interpreting unauthorized input. A bounded
input chunk/lookahead may cross the byte limit. Invalid/cancelled transports,
more than the discard bound, or a stalled peer may still see a reset/early
response: unlimited upload draining is deliberately unsupported.

`rejectionDrainBytes` configures 65536..268435456 bytes (default 67108864);
`rejectionDrainMs` configures 1..60000 ms (default 30000). These apply to both
entry and direct-stub refusals. Per-repository admission is checked before
this discard; a rejected upload does not own a queued codec job. A successfully
drained refusal leaves the connection usable. The generated large-refusal test
uses real Git and 32 MiB incompressible packs, HTTP streaming and gzip, both
object formats, under Wrangler R2/SQLite and direct workerd S3/SQLite. Git must
show the size-limit reason without broken-pipe/RPC/disconnect diagnostics. Rejection leaves refs/generation/log unchanged
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
The generated incremental fixture changes 80 small files; both object formats
prepare 53 fallback objects with two R2 GETs and zero temporary PUTs. Measured
time to first byte was 57 ms in the Node fixture; it is not a deployed latency claim.
Accounting backfill excludes these markers, including deferred-cleanup scratch.
Push reconstruction still needs quota headroom. Whole repository DELETE purges
all content, including scratch and objects outside the known ledger.

Fetch preflights output pack size and fallback reconstruction before 200.
Small fallback bodies stay in a dedicated non-evicting memory pin set, bounded
by `readPinBytes` (default 4 MiB, 0..16 MiB) and `readPinEntries`
(default 4096, 0..65536); empty/tiny bodies cannot evade the entry bound. Larger/overflow fallbacks use R2 read
scratch. Gateway defaults bound it to 32 MiB/request (`readScratchBytes`) and
64 MiB/repository including deferred cleanup (`readScratchRepositoryBytes`);
both are configurable from zero to the codec's 64 GiB outer temporary bound.
Scratch accounting/ownership precedes PUT and is released only after acknowledged
DELETE, independently of the stored-size policy. Token scratch defaults retain
the codec outer limit. `fetchPrepareMs` bounds lack of graph-selection/preparation progress
(default 30 seconds gateway, no new bound token; explicit 1..3600000 ms).
Successful bounded graph/object steps renew it; SQL selection yields between
128-node pages. Preparation checks before/after storage I/O. Known failures precede
headers. Uncancellable platform storage calls must settle before cleanup can
finish; no timeout releases ownership while a PUT may still land.

Gateway `wantPolicy` defaults to `reachable`: advertised tips, their authenticated
ancestry/tree/blob closure, or current persisted membership. Unproven older IDs
need index progress after a destructive publication: non-tip `git fetch origin <sha>`
wants are refused until alarms rebuild that membership (about a minute / 63
steps on the 25k default-packed fixture). The Git ERR asks the caller to retry. Objects
outside current reachability are refused with a Git-readable ERR before a pack.
Whole-catalog compressed-pack reuse is disabled in this mode, so it cannot send
extra unreachable objects. `wantPolicy: "catalog"` explicitly restores historic
whole-catalog wants; token mode retains that compatibility default.
**Rewriting refs does not erase stored bytes.** Packs, historical ref versions, push-log old IDs and backups remain. Even
after version pruning, the publication log retains old IDs. Unreachable-object garbage collection is not implemented. To
purge retained objects, an admin deletes the entire repository, completes the
resumable purge, and creates replacement content under a fresh namespace; the
old namespace stays permanently retired. Operators must also handle backups and
product copies. A force-push alone is not a physical data-erasure operation.

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
`examples/repository-host/cloudflare/src/host.mjs`. It exports createRepositoryHost
and ByteBudget. The factory returns `{ fetch, Repository }`: a Worker-compatible request handler
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
  maxConcurrentJobs: 32,          // active jobs, plus a bounded FIFO admission queue
  maxPendingRequests: 64
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
one billion proof edges, 65536 index work units/request, ten million refs,
1000 rows/page, 16 MiB/tree, 1 MiB each commit headers/message, 32 MiB total
metadata, 64 MiB/blob, 65536 path bytes, 1024 path components, 256 tag depth,
4096 parents, 65536 identity bytes, 8 MiB JSON, and 1 GiB each reconstruction
work/storage reads. Lower codec limits remain the hard outer limits. Raising
metadata/output limits increases memory and CPU use; raise the byte profile as
needed too. Ref-rooted reads are independent of total history size for a bounded
page/path, but cost grows with tag/path and reconstructed delta-chain depth; individual oversized objects, directories or pages
can still exceed request budgets and return 413.

The generated `browse-scale.test.mjs` compares 1,000 and 25,000 commits in both
formats, completes their raw indices, and checks immediate signed browser links
and fast-forward carry. Its additional default-packed fixture has 24 distinct
modules, a three-level path and delta chains up to depth 50, published in
500-commit increments using default Git packing (an active-repository workload).
Read assertions are
bounded in terms of requested rows/path and delta depth, not unrelated history;
all four ref-rooted operations write zero rows. Measured at 25k commits: the
undeltified SHA-1/SHA-256 fixture uses 14/70/18/18 SQL statements for commit,
20-entry history, nested tree and blob. The default-packed 150,028-object fixture
(depth up to 50) uses 14/70/70/76. The regression bound is 24 + 5 times the
sum of (1 + verified delta depth) for an **independent Git-selected reference
walk**, computed before the request. The fixture fixes the exact twenty commits
and root/path trees/blob; streamed work must equal that reference. Extra unrelated
walks cannot raise either bound. The realistic 1k-to-25k growth check permits
only the reference depth-dependent difference plus 32 fixed statements, with zero row writes. Full raw absence proof completes in **18**
requests for 100,003 undeltified objects, **63** for the default-packed wide/deep
fixture, including two deliberately 256-unit batches; remaining requests use
8192. The index is fully driven to completion in the test. Browser navigation
using returned proofs needs **zero index-building retries** immediately after
publication. Measurements include admission/lifecycle checks, not Cloudflare
billing rows or production CPU. An additive one-commit
publication needs at most one further batch, never a full history rebuild.

### Memory and shared-isolate scheduling

The gateway default is a **64 MiB payload pool**, shared by every Durable Object
of this factory in its isolate. This is deliberately smaller than the isolate's
128 MB limit: budget the embedding Rust/Wasm backend, its allocator, V8, HTTP,
SQLite, compiled code and GC in the remaining space. A byte ledger is not an RSS
limit. Measure the combined application before choosing a larger profile.
`isolateMemoryBytes` accepts 1..112 MiB; configurations unable to fit a job are
rejected. Lower caches/captures, codec controls, JSON/pages and pins together
when lowering the pool. Large configured pages or legacy extents can require a
larger pool, or receive retryable admission refusals.

Multiple factory instances must share a `memoryBudget` and the **same compiled
codec module object**. For example:

```js
const memoryBudget = new ByteBudget(64 * 1024 * 1024, 15000, 10000, {
  codecSlots: 4, readHeadroomBytes: 13 * 1024 * 1024,
});
```

The first arguments are bytes and admission-wait milliseconds. The third is a
validated legacy argument, now ignored; `memoryContentionMs` is also accepted
but has no effect. **No job is preempted for memory contention**: neither a
shared builder, cached reader, blob stream nor browse request. Deletion,
cancellation, transport idle/throughput limits and actual errors can still stop
jobs. Waiters receive 503 + `Retry-After: 1` after `memoryWaitMs` (default 15000,
1..600000), or when the isolate's 1024-waiter bound is full.

Admission rotates between namespaces and keeps FIFO order within a resource
class. A fitting light request can pass an oversized heavy waiter. Once another
namespace waits for heavy work, an incumbent cannot replenish its heavy leases;
its existing leases must retire naturally. This prevents a stream of small jobs
from indefinitely overtaking a larger co-tenant. There is no cross-tenant abort.
The default profile allows at most three active codecs per namespace, **two
heavy codecs per namespace** and three heavy codec jobs across the isolate.
The co-tenant heavy slot is never lent to an incumbent, even before another
namespace arrives. Namespaces holding no heavy lease are considered before
incumbents. With custom arena counts the per-namespace heavy ceiling is
codecSlots - 2; gateway profiles require at least three arenas. Admission still
requires sufficient working bytes; oversized profiles and several saturated
namespaces can time out rather than preempt anyone. Admission is **per repository
namespace**, not per product team: a team with many repositories receives many
turns. Admission-group/team keys are not implemented; the gateway must impose
its own group limits if needed. This key would only affect scheduling, never
authorization. With the default profile, the fourth arena remains available
for bounded commit/tree/history work. Long-lived jobs include Git discovery,
negotiation, pack intake, fetch generation and blob streaming. Small completed
advertisements release their codec before streaming their copied bytes.

The pool reserves four 6 MiB arena allowances and 13 MiB of working-byte
headroom when admitting data-phase jobs. Bounded control phases can borrow
working headroom while still obeying the arena and namespace ceilings; this
lets new cache hits parse behind many cached streams. A namespace cannot own more than
`pool - arena allowances - headroom` working bytes (27 MiB by default).
Consequently **one saturated namespace cannot prevent another namespace's
ordinary metadata read** (up to 13 MiB working bytes plus one arena) from being
admitted, or kill its live clone. Several saturated namespaces, raised read
limits and legacy 8 MiB extents can exhaust the pool; bounded waits/503s remain
possible. This is admission fairness, not a CPU, storage or network SLA.

Arenas are reused, including across object formats, and reset all parser,
source, graph, upload, pack and accounting state. Idle arenas stay charged;
dropping a codec and assuming GC freed its linear memory is not permitted.
There are at most four retained gateway instances (the bundled artifact uses
5.6875 MiB linear memory each, reserved as 6 MiB). `codecSlots` accepts 2..16
through `ByteBudget` (gateway factories require 3..16); its reserve and concurrency tradeoff must fit the pool.
Inactive objects retain bounded Store/SQLite metadata, not their own codec
pools. A legacy read reserves its worst existing chunk size **up front** using
an indexed maximum, never by queuing a top-up while holding a lease. An 8 MiB
extent adds up to 13 MiB with the default gateway cache profile. Legacy buffers
are copied to a 64 KiB fragment before that extent is released.

Default route reservations, including an active arena where needed:

| Work | Reservation |
| --- | --- |
| Commit metadata | 14 MiB |
| Directory/tree metadata | 17 MiB |
| History (2 MiB cumulative metadata, bounded JSON) | 19 MiB |
| Blob stream | 12 MiB by oid, 14 MiB by path |
| Refs or push-log JSON | 8 MiB; no codec |
| Git negotiation/advertisement | 11 MiB |
| Fetch generation | 14 MiB |
| Push intake/validation/report | 14 MiB + four times its validated control-prefix bytes |
| Upload transport staging | maxControlBytes + 1.25 MiB; no codec |
| Cached-pack reader | 192 KiB; no codec (three 64 KiB chunks) |

Reservations add working buffers to charged resident arenas; the same arena is
not charged twice. A phase releases its lease before acquiring the next.
`jobMemoryBytes` covers configured caches/captures, three chunk buffers, fallback
pins/descriptors and 1.5 MiB of metadata/frame headroom, in addition to an arena.
Push control/report copies are reserved separately from actual prefix length.
Browse uses the configured small caches/captures with owned-buffer reclamation,
and detaches parsed prefix buffers promptly. Its envelopes also scale with
configured metadata, JSON, parents, tree and cache bounds. Source payload defaults
(token / gateway) are: canonical cache 24 MiB / 512 KiB, representation cache
8 MiB / 256 KiB, chunk cache 16 MiB / 1 MiB, canonical capture 8 MiB / 256 KiB,
representation capture 1 MiB / 256 KiB, chunk writer 1 MiB, cache entries
8192 / 512, metadata aliases 256 / 128 and ref-name page 128 / 16 rows.
`memory` can lower these or raise gateway values to token ceilings; chunk size
is 64 KiB..1 MiB. Larger profiles reserve more bytes. Gateway browse cumulative
metadata defaults to 2 MiB; other `readLimits` defaults remain as listed above.
All limits remain within their documented codec/read hard ceilings.

**Token compatibility:** token mode does not use the shared byte pool by
default: **there is no isolate-wide memory bound in default token mode**.
Its prior cache sizes, unbounded pending queue, lack of new body or
admission deadlines, and lack of a throughput floor remain. Explicit
`isolateMemoryBytes`/`memoryBudget` opts it into bounded admission; configure a
suitable smaller cache profile if concurrency is required. With the default
four-arena/13 MiB-headroom pool, a token fetch reserves 69 MiB (63 MiB working)
and needs at least **100 MiB** total pool (24 + 13 + 63). Smaller configured pools
are rejected at configuration, including 96 MiB. A push also reserves four times
its actual control prefix, so needs correspondingly more space (the fixture
push/clone runs at 101 MiB); lower caches for large prefixes or concurrency.
At 112 MiB this legacy profile still fits only one heavy fetch. Custom arena,
headroom and cache values change the minimum and are validated together.
At most four idle token codecs are reused per factory: **24 MiB reserved /
22.75 MiB actual pinned-codec linear memory** even with no active requests.
Active codecs have no isolate-wide cap in this compatibility mode. Mixed
token/gateway factories must budget both profiles.

`maxConcurrentJobs` limits active processing jobs per repository (default 32,
1..1024). Cached readers release that slot and use a separate `maxCachedReaders`
ceiling (default 64, 1..4096), so a 30-person workshop does not leave only two
processing slots for browse or push. Size these ceilings for expected clients,
then size the byte pool for actual simultaneous generation/intake; increasing
job counts alone does not create memory. Beyond a ceiling, requests wait FIFO
up to `maxPendingRequests` (gateway default 64, 0..65536) and
`admissionWaitMs` (default 15000, 1..600000), then receive 503. Optional
`maxRequestsPerRepository` limits active plus pending jobs (1..65536).
Token defaults keep unlimited pending admission and no admission timeout.
Removed isolate job-count options remain rejected.

`workerd-memory.test.mjs` samples `Runtime.getHeapUsage` through the inspector
with two **live** generation streams, thirty live cached readers, and one
hundred further cache-hit fetch admissions, cancelled after their first chunk. It records JS used/total heap, embedder heap,
backing storage and the separately measured Wasm linear bytes, both before and
after forced GC. These are samples of a generated workload, not a measured
worst-case process RSS. The test checks live jobs, at most four retained arenas,
the 64 MiB ledger ceiling and a 96 MiB sampled combined envelope. Measurements
and repeatable commands are in [the latest review verification record](repository-host-review5.md).

### Concurrent clone pack cache

A Durable Object is single-threaded. Identical gateway full clones coalesce
into one generation-keyed pack build and then stream immutable framed R2 chunks.
The key includes epoch/generation, sorted wants, protocol/capabilities, want
policy and codec limits; authorization and wants are checked before lookup.
At most two distinct builds run per repository. Incremental, shallow and
filtered fetches have independent codecs/scratch. A conservative cached SQL
aggregate of the snapshot's published representations bounds full-pack size
before graph preparation. It includes duplicates/unreachable objects, so can
skip caching an otherwise fitting selection. A miss outside entry/total/count
capacity goes straight to one ordinary preparation: one walk per request, never
an abandoned extra builder walk. The bound requires no graph traversal, object
reads or repository-sized JS allocation.

The builder has its own job, snapshot, controls, codec lease and abort signal
**before selection/preparation begins**. It never belongs to its first client's
connection. Cancelling that client leaves followers running. Pre-header builder
failure lets each follower generate its original snapshot instead;
no unrelated admission/cancellation error is broadcast as a failed clone.
After headers, a storage/generation failure fails every follower of that build.
Shared caching requires negotiated sideband. Readers buffer incomplete pkt-lines
(maximum 64 KiB) and emit a fatal channel-3 packet at a complete framing boundary:
“git-zero: shared pack generation failed; retry fetch”. Git displays that remote
error; the incomplete pack is rejected and the client must retry. Raw non-sideband
requests use ordinary generation instead. This is not transparent resume/retry.
Builders are never preempted for another namespace's memory wait. When all
followers leave, a builder with no output for one response throughput window
(default 30 s) is cancelled and releases its slot/arena after bounded I/O cleanup.
A still-producing cacheable orphan may finish its bounded cache entry. Oversized
or full-cache requests never start a shared builder.

Gateway caching defaults on, token caching off (`packCacheEnabled`, strict
boolean/env selector; reference `PACK_CACHE_ENABLED`). Defaults: 512 MiB per
entry (`packCacheBytes`, 0..8 GiB), 1 GiB/repository (`packCacheTotalBytes`,
0..64 GiB), eight entries (`packCacheEntries`, 1..256) and **64 KiB** chunks
(`packCacheChunkBytes`, 64 KiB..1 MiB). A reader may consume ready chunks before
completion. The build bound is progress based: `packCacheBuildMs` is initial
grace (default 120000, 1..3600000), plus generated bytes divided by
`packCacheMinimumBytesPerSecond` (default 65536, 1..1048576). Thus 300 MiB gets
120 + 4800 seconds at the minimum rate, rather than a flat two-minute cutoff.
The 256-chunk test deliberately exceeds a shortened initial grace while making
progress. Underlying storage operations have their own bounded retries.

Cache and read scratch do not consume stored-size policy. Scratch exhaustion
under concurrent reads returns retryable **503 + Retry-After**, before headers.
Ownership precedes PUT; failed/stale caches are reclaimed in bounded alarm
pages after readers retire. DELETE cancels live jobs and waits for their I/O
before purge. No public cache URL bypasses grants. Cached clients spend a small
buffer/GET budget and no encoder CPU; repeated cold misses still share CPU.

A generated 25,000-commit / 150,028-object default-packed full gateway clone on
local workerd/S3 took **17.979 s selection + 2.753 s preparation**, with first
headers at 20.747 s and an 11,774,780-byte wire pack. Earlier unbatched runs exceeded the former 30-second total (162 s).
Scratch work now commits up to 128 graph nodes per synchronous transaction,
and full unfiltered traversal uses the pending depth index without unnecessary
shortest-distance layers. No transaction crosses an await. `fetchPrepareMs` now bounds **lack of progress**, default 30 s
in gateway mode (token has no new bound); successful graph/object work extends
it. Selection yields every 128 nodes and parsing/include-tag yield every 128
controls/refs. Work remains capped by codec object/work/byte limits. The pending
selection frontier has a partial depth index. There is no tested guarantee for
larger repositories or a production CPU/storage SLA. Identical full clones share
this cold work, but a new generation or non-cacheable request can pay it again.

Job-scoped SQL compiles each template once (bounded 256-template cache), rather
than repeating six regex rewrites per statement. The full-clone measurement
compiled **21** templates with **537,840** cache hits. A Node 24 microbenchmark
of 50,000 statements/sample measured a median 1.42 microseconds for the former
rewrite versus 0.016 microseconds for a cached template, excluding SQLite/I/O.
These microbenchmarks do not predict production billing or total clone time.
Snapshot tip wants use covering oid/peeled indexes, not a per-ref MAX scan.
Alarms prune up to 512 ref versions per step, preserving every interval visible
to the oldest live snapshot. The 5,000-distinct-tag/include-tag test also clones with real Git and models
10,000 closed versions, verifying indexed live/peeled lookup, yielding and safe
pruning. Live intervals are probed before historical intervals, so a heavily
pushed ref does not impose a scan of its retired versions on each current want.

A stalled response reader is stopped by `responseIdleMs` (10 seconds gateway,
60 seconds token, 1..600000). A slow active reader has no default total lifetime.
Gateway output requires 16 KiB/s over 30-second windows
(`responseMinimumBytesPerSecond`, 0..1 MiB/s; `responseThroughputWindowMs`,
10..600000). For example 300 MB at 5 Mbit/s takes **480 seconds** and can finish.
Explicit `responseTotalMs` (0..3600000, default 0) can truncate legitimate clones.
No contention timeout aborts an otherwise progressing reader.

The **request-body phase** is separate. Git input has 60 seconds idle, JSON
10 seconds. `requestBodyTotalMs` (gateway default 120000, 1..3600000) bounds only
control/negotiation or JSON, starting when reading actually begins, **after
admission**, never at arrival. Receive controls are Rust-validated, saved in
bounded SQL pages and replayed exactly, including gzip/one-byte fragmentation.
Admission waits between prefix and pack do not consume a transfer allowance.
Pack data uses `requestBodyMinimumBytesPerSecond` (gateway 16384, token 0,
0..1048576) and `requestBodyThroughputWindowMs` (default 30000, 10..600000), with
one window of initial credit. Only time waiting for transport input spends
credit; codec/storage work and memory waits do not. A 300 MB first push at
10 Mbit/s takes about **240 seconds**, and at 5 Mbit/s about **480 seconds**;
both exceed the old total but easily exceed 16 KiB/s. A below-rate trickle is
refused, while a progressing pack can exceed any number of windows.

Upload negotiation stages maxControlBytes + 1.25 MiB of transport/bind buffers,
then persists at most four 16-page SQL batches at the default 4 MiB control cap.
Fragmentation cannot force a durable write per arriving byte. Only admitted
heavy intake creates quarantine. Refusal tails drain outside admission and
codec reservations. At most two drains run per object: before authentication,
1 MiB/5 seconds; after authentication, configurable 64 MiB/30 seconds. Beyond
these bounds a client may still see a reset; discarded data is never parsed or
persisted. No drain retains a processing slot.

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
A separate module test holds responses and fills queues in two repositories
while a third completes independently. An admitted short grant survives its
own repository's queue; expired arrivals remain 404 and excess valid arrivals
receive 503 only for the saturated repository. Lower page/cache/capture/chunk bounds
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

Review regressions additionally cover hidden/read-only/wrong-stub/busy upload
draining, byte/time discard bounds, `oid:` of a rejected publication retained
after failed cleanup, DELETE cancelling concurrent live fetches after fencing, accounting-pending
fetch, cross-repository queue starvation, alarm prefix changes, and configured
jurisdiction identity. The earlier tree-depth/wanted-closure tests issue
sequential, correctly authenticated requests and drain each response; their
workload cannot fill the former four-job/64-request gate or reach hidden auth
replies. Removing that gate fixes concurrent idle-body interference, and the
new refusal tests reproduce and prevent the early-upload-reset mechanism.
Those facts do not establish the cause of the two historical socket resets;
the unchanged original tests are rerun, without relaxing assertions/timeouts.

The independent mixed-history benchmark reference disables automatic Git
maintenance: its explicit measured MIDX/bitmap and split commit-graph phase owns
those files. Detached geometric repacks in newer Git can otherwise rewrite/delete
its inputs outside that phase. A real-Git trace regression fails before this
configuration change and verifies no automatic writer, four retained incoming
packs, valid MIDX/graph, strict fsck and matching source IDs afterward. Trace
instrumentation is opt-in for the regression and absent from measured runs.
