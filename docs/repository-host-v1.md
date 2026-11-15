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
`storedBytes` and `lastPublication`. Counts come from the catalog; storedPackBytes
is the sum of published encoded pack sizes, with no deduplication credit. Actual
physical R2 bytes (`storedBytes`) are unknown cheaply and therefore null. Times
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
is required on a cold instance. No deployment cost is inferred from this.

## Scope and verification

Access levels, grants and explicit lifecycle are implemented. Browse APIs,
durable actor/push records, configurable policy and a configurable host-module
boundary remain follow-up milestones. The actor is authenticated
and available at the authorization seam but not yet persisted. Read grants,
like token access, currently cover the published Git catalog, including older
ref generations; there is no reachability-restricted browse API yet. LFS, SSH,
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
