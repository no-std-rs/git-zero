# Frozen strict variant: bytes and SQL ledger

This is a reviewable paper specification for the **rejected fully journaled transparent-legacy-writer variant**, not a codec implementation. SQL row lower bounds below are deliberate design costs; exact cursor counts, databaseSize, crash recovery liveness and runtime behavior remain unmeasured. The separately mentioned new-Native-only writer contract is outside this format's rejection.

## Durable encoding

Use the existing `repository_state.catalog_root BLOB`, no separate map row/table. Fresh isolated new namespaces create the existing three tables with their already supported columns present from creation: `repository_state(id INTEGER PRIMARY KEY,generation INTEGER NOT NULL,manifest TEXT NOT NULL,catalog_root BLOB)`, `blob_catalog(key TEXT PRIMARY KEY,size INTEGER NOT NULL,peak_bytes INTEGER NOT NULL DEFAULT 0,entries BLOB) WITHOUT ROWID`, and `high_water(id INTEGER PRIMARY KEY,bytes INTEGER NOT NULL)`. No migration of existing namespaces is in scope. Fresh legacy-format repositories under the new Worker retain GZIR-v1/page-v1/v2 semantics; this changed common schema/creation work must be measured in feature-off controls too.

Container activation uses **GZIR v2**, fixed268-byte header followed by `n` sorted physical entries of36 bytes each. All integers are little endian; u64 values must fit the existing JS safe nonnegative integer domain. Reject unknown versions, nonzero reserved bytes, length mismatch, inconsistent state/fields, duplicate/unsorted physical digests or invalid alias masks. Exact length is `268+36n`, `0<=n<=32`, maximum1,420 B.

| Byte offset | Width | Meaning |
| ---: | ---: | --- |
| 0 | 4 | ASCII GZIR |
| 4 | 1 | version2 |
| 5 | 1 | physical entry count n |
| 6 | 8 | durable known actual physical peak H |
| 14 | 8 | applied physical-debt generation |
| 22 | 1 | mode:0 inline;1 paged |
| 23 | 1 | container state:0 no map;1 planned;2 present;3 physical deletion debt |
| 24 | 1 | logical alias availability mask, bits0..2 only |
| 25 | 7 | reserved zero |
| 32 | 8 | container creation generation, initially1 |
| 40 | 8 | physical-container debt generation, zero absent debt |
| 48 | 16 | original authenticated container-upload nonce |
| 64 | 32 | physical content SHA-256 |
| 96 | 4 | physical total T, `32MiB<T<=48MiB` |
| 100 | 96 | ordered three logical SHA-256 digests |
| 196 | 32 | pending PUT physical digest |
| 228 | 4 | pending PUT body length |
| 232 | 1 | pending kind:0 none;1 container;2 ordinary |
| 233 | 3 | reserved zero |
| 236 | 8 | pending PUT expected/current generation |
| 244 | 16 | pending PUT attempt nonce |
| 260 | 8 | previously cataloged size of pending physical key, zero absent |
| 268 | 36n | physical SHA-25632B + u32 size4B, sorted unique |

The container key is `chunks/<physical SHA>` in the fenced new bucket/repository. Original logical keys are `chunks/<logical SHA>`. Logical offsets are fixed0,16,777,216,33,554,432; lengths16,777,216,16,777,216,T−33,554,432. Pack/index split is the unchanged authoritative StoredPack split. A canonical map digest is SHA-256 over a domain/version, repository, creation generation, nonce, physical digest/T and ordered logical digests/derived ranges; the upload MAC and receipt bind it. It is derivable, not a separately stored map-hash field. Existing manifest fingerprint authenticates logical contents; it does not authenticate this opaque physical index.

State0 zeros all container fields. State1 is non-readable and cannot back a nonempty published manifest. State2 requires exactly one cataloged full container member while physically present, with alias availability distinct from current manifest liveness. State3 retains full map/size/debt until confirmed physical absence and durable removal. Pending kind0 zeros its fields; nonzero kind blocks mutation/DELETE everywhere until reconciliation. Do not treat pending length as measured physical existence or add it to known H: pending live/peak closure is unknown until physical completion/inventory is established.

Every normal physical entry is <=16MiB; the mapped container entry alone may be <=48MiB. Fully comparable inline encoding grows by `268−22−2×36 =174 B` when replacing three physical root entries with one. That is raw payload arithmetic only. The4,923 B final manifest, record headers, free-space fragmentation and overflow pages require an actual SQLite geometry test. All profiles must remain <=24,821.76 B allocation; any added4KiB page fails.

For demotion keep a v2 root with mode1, n0 and all map/journal/H fields; **never set catalog_root=NULL and lose the map**. Physical pages use **GZCP v3**: existing4-byte GZCP magic, version3/count bytes,8-byte applied physical-debt generation, then36-byte sorted digest/u32-size entries; exact length `14+36n`,1<=n<=32, maximum1,166 B. Only the root-bound container may exceed16MiB. Validate this at physical lookup before mutation; aggregate SQL recognizes v3 exact shape/length but does not alone certify payload membership. Reopen, peak extraction, aggregate count/sum, split/demotion, debt replay and GC rebuild explicitly decode v2/v3. Existing v1/v2 decoders remain for ordinary legacy repositories; unknown/cross-format ownership fails closed. Split uses existing sorted radix-prefix rules, depth<=64. Root persists as a zero-entry owner in paged/empty states, including after container removal.

## SQL templates and actual ownership

The following templates define this strict design. `sync` is an awaited storage barrier, not another fabricated SQL statement. Actual data/schema cursor rows, databaseSize and all failed operations must be retained; schema/internal costs are not assigned zero. Branch/cardinality-dependent totals are explicit below rather than invented fixed counts.

| ID | Statement / cardinality |
| --- | --- |
| C1–C3 | The three exact CREATE TABLE IF NOT EXISTS schemas above, once per DO construction; no ALTER in a genuinely new namespace. |
| C4–C5 | `PRAGMA table_info(repository_state)` and `PRAGMA table_info(blob_catalog)`. |
| C6 | Existing physical `blob_catalog` SUM/count/max/invalid/deferred aggregate, extended to recognize GZCP-v3 shape and safe sizes; one statement reading actual existing rows. |
| C7 | `SELECT bytes FROM high_water WHERE id=1`. |
| C8 | `SELECT generation,catalog_root FROM repository_state WHERE id=1`; decode map/journal before accepting requests. |
| C9 | `SELECT manifest FROM repository_state WHERE id=1`; derive current logical/debt context. Pending/invalid format never becomes catalog-certain from empty SQL alone. |
| A | `SELECT generation,manifest,catalog_root FROM repository_state WHERE id=1`; current authority before mutation/GC. |
| I | Existing row: `UPDATE repository_state SET catalog_root=? WHERE id=1 AND generation=?`; first row: `INSERT INTO repository_state(id,generation,manifest,catalog_root) VALUES(1,0,?,?)`, with canonical empty generation-zero manifest. Sets exact pending intent; verify one matching row, then sync **before R2 PUT**. |
| P | `SELECT generation,manifest,catalog_root FROM repository_state WHERE id=1`, then `UPDATE repository_state SET generation=?,manifest=?,catalog_root=? WHERE id=1 AND generation=?`; transaction, then sync. Winning next state atomically owns map/physical membership/H and clears intent. A stale known-success PUT instead updates catalog_root while preserving current generation/manifest, then syncs; it owns an orphan, never authorizes client deletion. |
| L | Standalone legacy PUT completion: `UPDATE repository_state SET catalog_root=? WHERE id=1`; owns physical entry/H and clears intent, then sync. Separate later CAS uses P and does not erase map/journal. |
| R | Logical alias-only retirement/resurrection: `UPDATE repository_state SET catalog_root=? WHERE id=1`; sync. If already represented by the current P transaction, no second write. |
| H | Existing high-water upsert using MAX, or replacement if fallback invalid; one row write when a stronger durable witness is required, then sync **before physical DELETE**. |
| B | Existing physical page upsert for each page actually created/rewritten; DELETE each removed catalog row. Sorted physical catalog lookup uses existing key/ancestor UNION batches and actual rows, not a free map-row lookup. |
| G | After physical GC: `DELETE FROM blob_catalog`, then inline root UPDATE once; or paged root UPDATE once plus one B per rebuilt page. One transaction, sync; keep durable debt/uncertainty if any operation fails. |

Constructor/reopen uses C1–C9 on activated inline storage: nine statement calls, not nine claimed billed rows. Root-memory decoding adds no independent map SELECT; pending recovery adds actual A/HEAD and any required inventory, checkpoint and cleanup writes. A clean reopened DO can mutate only because every prior physical PUT used I and cleared it durably; ephemeral registration/Native state is not evidence of that invariant. Corrupt root or a committed container manifest without its map cannot be repaired by deleting unknown keys.

For unambiguous statement identity, activated paged storage has only GZCP-v3 rows; initial activation requires an empty catalog. C6 is the following aggregate (malformed totals are rejected again in JS safe-integer validation). Ordinary legacy storage retains its frozen R20 aggregate instead:

```sql
SELECT COALESCE(SUM((LENGTH(entries)-14)/36),0) AS objects,
       COALESCE(SUM(1.0*size),0) AS bytes,
       COALESCE(MAX(peak_bytes),0) AS peak,
       COALESCE(MAX(CASE WHEN entries IS NULL
         OR hex(substr(entries,1,5)) <> '475A435003'
         OR LENGTH(entries) NOT BETWEEN 50 AND 1166
         OR (LENGTH(entries)-14)%36 <> 0
         OR hex(substr(entries,6,1)) <> printf('%02X',(LENGTH(entries)-14)/36)
         OR typeof(size) <> 'integer' OR size NOT BETWEEN 0 AND 9007199254740991
         OR typeof(peak_bytes) <> 'integer' OR peak_bytes NOT BETWEEN 0 AND 9007199254740991
         THEN 1 ELSE 0 END),0) AS invalid
FROM blob_catalog
```

B is `INSERT INTO blob_catalog(key,size,peak_bytes,entries) VALUES(?,?,?,?) ON CONFLICT(key) DO UPDATE SET size=excluded.size,peak_bytes=MAX(blob_catalog.peak_bytes,excluded.peak_bytes),entries=excluded.entries`; removal is `DELETE FROM blob_catalog WHERE key=?`. H is `INSERT INTO high_water(id,bytes) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET bytes=MAX(high_water.bytes,excluded.bytes)`; an invalid fallback uses `bytes=excluded.bytes` in that same upsert. The aggregate's shape check does not replace decodePage's full key/order/member/size/generation checks before using a physical page. Paged lookup preserves the exact R20 key-plus65-ancestor UNION groups of four (17 statements), with v3 decoding; its actual read rows and page payload owners remain charged.

**Initial successful inline upload:** C calls if reconstruction; A validates empty current authority and physical certainty; I +sync; one R2 PUT; P +sync; no retirement DELETE. At least2 data-row writes; schema/reads retain actual counters. Existing paid authoritative startup inventory may establish initial empty certainty, but no newly required LIST is free. If no valid inventory/intent history establishes emptiness, return ineligible/uncertain before mutation and charge any requested recovery.

**Each subsequent one-PUT fused push:** A, I +sync, one R2 PUT, P +sync. At least2 data-row writes even if every other catalog/CAS action is folded into P. Ordinary content-addressed PUT + separate CAS needs I, L and P, at least3, plus actual catalog-page work if paged. The map is carried in the existing row update, not rewritten as a separate row; that still does not remove I. This covers old Native mutation paths transparently and is the source of the concrete cost rejection.

**Non-PUT CAS:** A, conditional P write +sync only on a matching generation; stale authority has no write. Root physical reachability/debt and alias state are computed from complete next logical live keys inside that publication transaction. Equal-content mapped PUT verifies bytes and physical certainty; when alias already available, zero physical PUT/intent and zero new write; resurrection uses R. No existing one-PUT proof is issued.

**Debt/GC:** A rejects stale generation and compares supplied logical live keys with the complete authoritative set, then derives physical reachability. If an intent is pending, first establish provider quiescence; a negative HEAD alone is insufficient. Charge each physical HEAD and each LIST page. Compute pre-delete distinct actual physical bytes including duplicates/orphans/failed-delete survivors; execute H when needed and sync. Perform actual physical DELETEs only for unreachable keys. Confirm/retain debt and G +sync; failed/unsettled work leaves uncertainty. Alias-only retirement while another range lives uses R, **zero physical DELETE**, full retained storage. Last-alias CAS already records physical debt before deletion; do not invent one physical DELETE per logical alias. All proof optimizations are suppressed in activated repositories; paid reconciliation uses physical keys/sizes. Normal observer LIST remains observation, not an automatic certificate that clears a pending in-flight write.

Demotion uses one root mode change plus one upsert per resulting physical page in a transaction +sync; further splits use actual delete/upsert counts. Paged ordinary PUT uses I, B and the root intent-clear/H update plus CAS as applicable, all charged. No paged route can silently discard mapping, physical debt or pending intent to recover an inline cost.

The least possible successful scheduled64 ledger under these rules is64 SQL row writes/profile versus measured R20's39. Exact state geometry and read/schema/GC counts remain NULL; no missing cost is set to zero. The paper lower bound already fails the conservative known-cost margin, so no runtime implementation or benchmark follows.
