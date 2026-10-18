# Inline catalog and publication coupling

This opt-in local Worker candidate stores one bounded catalog root beside the
manifest. The Rust Git codec, compressed R2 pack chunks, freshness checks and
existing memory, upload and snapshot limits stay the same. It has not yet been
measured in the four-profile cloud-VM screen; local correctness receipts are not
a net-cost or speed result. Full billing still lacks Worker CPU, DO duration,
monthly SQLite retention and the external Native codec's compute cost.

Registration requests `catalog_inline:1` together with the existing upload,
peak, pages and deferred capabilities. The exact acknowledgment is
`{"version":1,"members":32,"manifest_bytes":8192}`. An absent acknowledgment
keeps the previous upload protocol. Native rejects an invalid present
acknowledgment before installing any session capabilities. The gateway strips
public internal headers and derives eligibility from its private registration.
The GZUP frame and upload endpoint are unchanged.

## Ownership

The first eligible mutation adds nullable `repository_state.catalog_root BLOB`.
DDL and migration cursor costs belong to the real mutation. A non-NULL value
owns the complete catalog; the normalized table contains no second root or
descendant membership. NULL selects the existing scalar/page representation.
The independently versioned GZIR v1 encoding is 22 bytes plus 36 per member,
at most 1,174 bytes: magic/version/count, safe-u64 peak, safe-u64 acknowledgment,
then sorted unique SHA256 digests and u32 sizes bounded by 16 MiB. It contains
no object payload. Count zero is valid and retains the peak witness.

Every mutation and recovery path uses the same ownership abstraction. Durable
decoding is unconditional after schema discovery, including old Native
registrations, registry loss and DO eviction. Reads and observers neither
migrate nor acknowledge, repair or certify physical state. A sole normalized
root can move atomically into the singleton, with both changed rows charged.
Descendants and mixed shapes keep the previous representation. A 33rd member
or a valid manifest exceeding 8,192 bytes causes a paid, synced demotion before
the ordinary operation. Larger public manifests and scalar retirement above
256 keys remain accepted. These are optimization eligibility limits, not new
public API limits. A Worker rollback requires deliberate charged demotion
before deploying code that cannot decode this new durable format.

## Atomic publication

On an eligible upload the Worker validates the complete frame, content hash,
manifest and bounds, resolves only eligible old debt, then performs the real
immutable R2 PUT. A synchronous SQL transaction reads the authoritative current
generation and writes one singleton row containing the new root and peak. A
winning CAS replaces generation and manifest in that same row; a stale CAS
keeps the current manifest while durably retaining its uploaded orphan and
temporary physical peak. A successful sync precedes the winner response and
every ordered retirement DELETE. Stale CAS-only calls retain zero R2 work.

This replaces the old *separate catalog-sync-before-CAS* barrier with atomic
coupling. No recovered durable winning manifest can lack the corresponding
catalog and peak witness. The existing R2-PUT-before-first-metadata failure
window remains: a failed or ambiguous transaction/sync makes the current view
uncertain and requires generation-checked physical GC. Native never retries
scalar PUT/CAS after an attempted ambiguous fused request. It must recover
using real LIST, peak preservation and synced catalog rebuild before its next
mutation. External independent PUT and CAS retain their separate valid calls
and durable barriers.

The root acknowledgment covers **prior resolved debt**, never the new
publication's not-yet-executed retirement. Same-key resurrection still persists
and syncs the old acknowledgment before physical PUT. This prevents replaying
an old all-success absence proof over a resurrected object after a later SQL
failure. All real DELETE outcomes remain ordered and charged. After a partial
DELETE, eager catalog reconciliation may stamp the current generation because
it records actual successful outcomes and retains failed members; the Native
view stays dirty and no all-success proof is issued. Transaction failures,
unknown cursor counts and failed or malformed responses never fabricate known
SQL counts or proofs.

## Recovery and measurement

Malformed, overlapping, unsafe or future-ack ownership leaves manifest reads
and generation-checked GC available while blocking ordinary mutation. GC uses
the full physical inventory, preserves independently valid historical peak
witnesses and the complete pre-delete physical/orphan floor, then atomically
rebuilds exactly one ownership representation and syncs. Arbitrary lost or
corrupted historical witnesses cannot reconstruct unobserved past peaks.
Observer LIST is never used to clear this uncertainty.

Rust owns the ignored actual workerd/R2/SQLite contracts in
`examples/repository-host/tests/fused_metadata.rs`; no MJS runner is used. The
early prototype used real 11/13-key layouts, 11 packs, 33 refs and 5,267/5,461
byte manifests. Promotion, normal write, GC and process reopen stayed at
24,576 SQLite bytes. It showed one actual normal SQL written row against two
for independent PUT/CAS, with identical real R2 work. Its GC retained all
objects; the production storage gate separately removes actual orphans and
checks the 13-to-11-key retirement shape and reopen. First-write DDL,
transitions, fallback and recovery remain paid. Fresh benchmark receipts must
establish the net saving and unchanged resource/latency guards.
