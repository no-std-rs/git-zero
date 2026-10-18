# Gateway preparation for small receives

The optional small-receive path combines the existing authoritative generation
check and first Full snapshot batch in one DO request. It preserves the same
compressed packs, object GETs and bytes, strict native Git receive transaction,
and atomic catalog/publication durability. Local workerd correctness controls
are separate from the cost/performance screens; no measured net price or speed
claim follows from this design.

Cold codecs request `gateway_first_receive: 1` alongside first-read and generation
witness support. Its exact version/body/frame/session acknowledgment is optional
for old Workers, but a present malformed or unsolicited acknowledgment fails the
registration after resetting prior capabilities. Old first-read codecs remain
fetch-only. Warm codecs request neither optimization. The local runner's
`--gateway-first-receive off` supplies a same-source control through the normal
single registration handshake.

Only exact v0 receive POSTs with the normal MIME type, identity encoding, bounded
headers and a declared body of at most 128 KiB can be prepared. Native cheaply
recognizes supported update pkt-lines, valid ref names, a flush and nonempty PACK
framing with a supported version and minimum trailer. Strict Git still validates
the complete pack, commands, thin bases and refs. Exact `0000`, ref-only/no-PACK,
large streamed pushes and unsupported valid framing use the ordinary path before
any prefetch GET. The ordinary empty-probe shortcut and input caps remain intact.

Planning performs no Git operation, storage mutation or materialization. It
validates all current snapshot metadata and selects every pack. The gateway uses
its private authenticated stateless envelope to bind repository, registration,
nonce, original request and exact selection. The DO checks actual generation and
the full published manifest fingerprint before reading the first batch. Native
rechecks its matching local fingerprint and exact Full selection before consuming
the verified prefix once, then reads all remaining batches normally. Generation
changes use the ordinary charged path with zero stale-plan GETs; local changes
after prefetch retain and discard the paid bytes before safe fallback. Corrupt or
auth-invalid frames fail without a hidden retry.

After complete Full materialization, only that authenticated continuation creates
an owned, non-Clone `ValidatedFullReceive`. The transaction validates and consumes
it once to skip exactly its first unconditional cold reconstruction. Ordinary
receives pass no token and keep their existing policy. No blob cache, spool,
session map, global prepared flag, new SQL representation or increased cap is
introduced. Published and local manifest digests remain distinct after retirement,
when durable garbage and locally cleared garbage differ.

The existing receive wrapper still transfers lineage/advertisement ownership,
tracks publication activity and pending metadata, cleans old debt, creates strict
quarantine alternates, runs Git, compacts bounded inputs, imports immutable packs,
publishes/syncs before retirement and invalidates response caches. Prepared push
responses are never cached. Remaining reads and first-batch receipts use push
phase; preparation timing includes Full materialization. Logical request/input/
output counters run once, including the response from an attempted transaction
that fails. Pure planning/auth overhead is not a public Native operation.

The continuation is bounded by 20 framing bytes, 1,500,000 metadata bytes, a
131,072-byte original request and a 16 MiB payload plus 2,060-byte bulk frame:
18,410,368 bytes total, below the existing 64 MiB incoming cap. Snapshot limits,
256-entry bulk limit and the joint 32 MiB owning metadata/cache budget remain.
Additional fixed negotiation state is charged inside that budget.

An activated receive removes one service Worker and one DO callback. All R2 and
SQL work must be measured from actual receipts; the structural 48-call opportunity
in the declared interleaved workload is not measured savings. Additional crypto,
Native HTTP stages, Worker CPU, DO duration, monthly SQLite and external Native
compute remain unpriced. Full bill remains NULL. Independent source review,
fresh four-profile screens and the original/final five-pair validation determine
whether this candidate is useful.
