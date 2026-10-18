# Proposed peak watermark in catalog writes

The [independent design](design.md) carries the durable peak in each already
written PUT catalog row. A last-witness delete or destructive GC first preserves
the peak in the existing fallback row and syncs. Orphans, failures, restarts,
legacy catalogs and registry loss require explicit recovery contracts.

Successful recorded R5 traces have 404 fixed SQL writes: 132 PUT rows, 84 DELETE
rows, 128 manifest rows and 60 high-water rows. The normal proposed path can
remove those 60 separate peak writes without extra R2 calls. Gross projected
saving is $0.060 per 1,000 four-profile replays, before actual migration, schema
read and fallback costs. Projection attribution is inferred from successful
present-key normal traces; implementation must verify actual SQL cursor rows.
It is not a measured candidate price or a complete cloud bill.

Only the new Native requests the capability in its existing registration. The
common Worker keeps old fresh namespaces on v1 and mints private service headers
from its registry. Mutation-only migration enables v2; reads never migrate.
Existing v2 repositories keep safe behavior after registry loss or old Native
reconnection. Rolling back to an old Worker needs an explicit peak checkpoint.

Independent design review found no fundamental blocker. It requires explicit
post-error uncertainty: SQL witness reload alone cannot certify physical totals;
existing authoritative reconciliation must precede later ordinary mutation
accounting, including direct/old callers. Actual SQLite pages remain a gate:
24,576 to 28,672 bytes would fail the unchanged 1% guard. Real fault, migration,
process-restart and fresh paired benchmark evidence remain required. No
implementation, acceptance, speed win or plateau is claimed here.
