# Asynchronous selected-pack cache

The read-only Worker host can suspend a prepared upload-pack request while it
loads or publishes a complete selected pack. Its trusted loader enables caching
explicitly. Metadata snapshot KV/R2 storage and durable read/write repository
hosting use separate contracts.

```js
import { createPackCache } from "../examples/cloudflare-worker/src/pack-cache.mjs";
import { createRepositoryWorker } from "../examples/cloudflare-worker/src/repository.mjs";

// Construct once per isolate and authorized repository namespace, then reuse.
const packCache = createPackCache({
  backend: "r2",                  // or "kv"
  binding: env.SELECTED_PACKS_R2,  // or a KV binding
  namespace: "public-view-v1",    // explicit host-owned private namespace
  maxBytes: 147456,
  ttlSeconds: 3600,
  maxInflight: 16,
  maxWaiters: 64,
});
const worker = createRepositoryWorker(module, async (request) => {
  const selected = await loadAuthorizedImmutableRepository(request);
  return selected && { ...selected, packCache };
});
```

Bindings and authorization are host-owned. A loader returns the same existing
`repository`, `target`, optional want policy, revision limits and publication
settings. Caching grants no new object access. Disable it by omitting `packCache`.
The metadata demo does not silently bind a cache from captured environment data.

## Ownership and phases

Each request imports a validated immutable raw repository into its own Wasm
instance. Before backend I/O, `cache_prepare` validates the route, capabilities,
wants, duplicate-counting occurrence limits, history/filter selection and graph
budgets. It retains selected indices and request-specific control bytes. The
original request frame and mutable JavaScript repository are no longer consulted.

Non-pack replies bypass storage. Pack replies expose the existing versioned
48-byte `GZPC` selected-set key. It includes format, codec, encoder revision and
the exact canonical unique ID set. Equivalent object sets can share raw packs
across request order, protocols and sideband modes. These keys are independent
of Git's checksum-based public pack filenames.

On a miss, bounded pulls generate raw PACK bytes without exposing Git success.
Before hit output or miss insertion, `cache_attach` verifies stored framing,
checksums, canonical IDs and the exact selected set. A miss publication must
finish successfully before a Response is returned. The retained ACK/shallow/
sideband control is framed for this request; it never enters cache records.

The generic Rust `PreparedResponse` has the same boundary. Its caller retains
the authorized repository, selected slots and control buffer across awaits.
It borrows neither the original HTTP body nor temporary validation metadata.
Separate storage permits overlapping preparations; an unread response is required.
Dropping a preparation cancels it without output. Failed attachment consumes it.

## Records and limits

Keys are stored at `selected-packs/v1/NAMESPACE/KEYHEX`. A record contains eight
magic/version bytes, big-endian u64 expiry, the full 48-byte key, u32 pack length,
complete raw pack and a SHA-256 digest over the preceding record. The 100-byte
overhead counts against backend read bounds. Schema, key, length, expiry range
and record checksum are validated before pack attachment. Expiry is inclusive.

The maximum raw pack is 144 KiB, covering complete 64-object/128 KiB resident
imports with stored encoding overhead. Configured byte limits may be lower.
Retention is 60 through 31,536,000 seconds. Namespace names contain one through
64 ASCII letters, digits, underscores or hyphens. Producer admission is one
through 1024 active keys; waiter admission is zero through 4096 requests.

KV reads request a stream; R2 reads consume `body`. Both are bounded before a
complete record is retained. Oversize streams are cancelled; R2 size metadata
can reject before reading. Every hit is copied into its own instance, so later
eviction or backend mutation cannot change an active response. Writes publish
complete immutable records, and the adapter awaits the put. Backend bindings
must provide complete-value atomic publication.

KV receives `expirationTtl`. R2 expiry is logical; the adapter does not delete
old objects. Configure host-owned R2 lifecycle cleanup. Corrupt records and
backend errors fail closed without silently regenerating or returning Git success.
Missing/expired/evicted records generate a verified replacement.

## Concurrency, cancellation and consistency

One cache object coordinates same-key work within its isolate and namespace.
It registers a flight before the first read, bounds active producers and queued
waiters, and releases state on every success, error or cancellation. A follower
rechecks storage and authenticates the pack against its own prepared request.
It shares neither a response cursor nor mutable pack bytes with the producer.

This is local coordination. Another isolate can produce the same immutable key.
KV propagation may cause a later miss after a completed local flight. There is
no distributed lease, compare-and-swap, global quota or production retention
scanner. Independent duplicate complete-value writes are safe because every
candidate must match the same exact set and encoder contract.

Request aborts stop awaiting cache work and cancel the Wasm session. An already
started atomic backend put may finish publishing its valid complete record.
Cancelling one follower does not cancel another request. A returned response's
body cancellation releases only its own stream. Retry begins with fresh routing,
authorization and selection; warmed entries never bypass revoked want permission.

`statistics()` returns a frozen snapshot of hits, misses, waits, writes, failures,
active/peak producers, flights and waiters. These are isolate-local counters.
Local tests establish correctness under deferred reads/writes, failure, expiry,
eviction and cancellation. Production CPU, JavaScript/isolate memory, backend
propagation and total cost still need deployed measurements.
