# Durable catalog-page contract

Both exact registration integers `catalog_peak:1` and `catalog_pages:1` are
required to acknowledge/mint the pages capability. Registration has no DO call.
The first fully validated catalog mutation adds the peak column when needed and
then nullable `entries BLOB`; every actual ALTER/read/write stays in that request.
Reads, invalid requests and stale GC never migrate. Old fresh registrations keep
their existing v1/v2 behavior on the same Worker.

Durable `entries` discovery governs every subsequent client, including older
Native registrations, lost registry state and process restart. Existing canonical
legacy members retain NULL entries and update in place. Worker rollback to code
that cannot decode pages is unsupported; Native rollback remains supported.

The page format is `GZCP`, u8 version 1, u8 count 1–32, followed by sorted unique
32-byte digests and little-endian u64 sizes. Length is exactly `6 + 40*count`,
at most 1,286 bytes. Each size and aggregate must fit a nonnegative safe integer;
zero-length scalar blobs remain supported. Private `pages/<hex-prefix>` SQL keys
never pass the public blob validator. No payload enters SQLite.

Lookup checks the exact legacy key and all 65 digest ancestors, retaining bounded
page scratch and failing closed on duplicate membership, overlapping leaves or
invalid encoding. Actual local cursor evidence rejected the temporary-key read
cost of `IN`; batches of four independent `UNION ALL` PK equalities read only
matching rows and stay within the existing compound-select limit. Missing routes
use indexed binary predecessor/successor queries. Overflow partitions only 33
members after their common prefix into 2–16 nonempty children, atomically carrying
the peak to every child. Empty pages are removed; churn retains no routing rows.

Successful R2 PUT precedes page/legacy SQL and sync, which precede manifest CAS
and its sync. Retirement then preflights the union of all rows that could vanish.
A witness must survive outside that entire union, or fallback checkpoint/sync
completes before the first real DELETE. Ordered R2 attempts retain one truthful
outcome per eligible key. Confirmed members are removed in one SQL mutation per
touched page in the same callback; failed members remain. SQL, transaction or
barrier ambiguity returns an HTTP error and invalidates certainty. Native's
existing dirty recovery handles errors and partial receipts before later work.

Generation-checked GC checkpoints the entire physical pre-delete inventory,
including unrecorded upload orphans, before deleting or resetting SQL. Recovery
can replace corrupt/duplicate membership without decoding it first. Retained
canonical keys rebuild through bounded page builders; arbitrary physical orphan
names never address private SQL page keys. GC and metrics add no hidden repair
to normal reads. Actual failed cursor counts remain null; they are never zeroed.

A possible future CPU refinement is a maintained scalar page-row count allowing
a proven sole root leaf to finish ancestor lookup early. R12 keeps the simpler
complete bounded probe: no measured CPU benefit yet justifies extra fault state.

Packing does not strengthen the R11 cross-process gap contract: an R2 PUT that
never obtained a durable SQL witness requires Native startup or explicit direct
client authoritative GC after an ambiguous operation/restart. No per-PUT intent,
new normal R2 operation, larger cache/budget or retained catalog-wide map is added.

Malformed numeric metadata also requires authoritative recovery. Page reload
rejects negative, unsafe and fractional size/peak values, retains the maximum
of individually valid peak witnesses, and flags invalid inventory/fallback state
without preventing construction or GC. REAL summation avoids SQLite integer SUM
overflow; the resulting nonnegative integer total must still fit the safe range.
An invalid fallback is replaced and synced before deletion, even at a zero floor.
Valid ordinary rows keep their existing SQL row counts and fallback-write path.

Recovery preserves the maximum of all still-valid durable historical witnesses,
the process floor and the complete physical pre-delete inventory. If arbitrary
corruption destroyed every trustworthy historical witness, an older historical
maximum is unknowable; current physical inventory cannot reconstruct it. This
corruption limit is separate from the R11 unrecorded-upload crash gap above.
