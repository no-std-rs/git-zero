# Reviewed next-candidate designs

These files preserve design-time evidence and conditional hypotheses. Statuses
inside older records describe their writing time. The [benchmark report](../../../../hosting-interleaved-benchmarks.md)
records subsequent implementation and measurement.

The R13 replay reproduces all 704 measured rows and its known fixed objective.
R14–R17 designs, independent reviews and implementation preparations retain their
original bytes. Their fresh screens are published separately; none establishes
a speed win or global minimum.

The [protected-chunk design](protected-chunk-cost-design.md), independent review
and preparation describe one bounded immutable GET cache. Its exact R18 source
and ledger are approved. The [fresh screen](../ci-r17-r18-fresh-screen/README.md)
measures a 5.0936% known-cost reduction; five latency groups fail, so it is only
the provisional cost comparison anchor.
It is absent from the completed R15–R17 screen.

The [multi-chunk audit](multi-chunk-next-design.md), [independent review](multi-chunk-independent-design-review.md)
and [implementation preparation](multi-chunk-implementation-prep.md) extend the
hypothesis to first-fit secondary admission under the same memory budget. The
modeled $0.12168 extra GET saving is conditional, with a zero unconditional net
lower bound. A hard 14 MiB response cap is rejected; eligible responses up to
16 MiB may reclaim chunks under actual joint pressure. Pruning requires trusted
current Full authority. The preparation's provisional source snapshot is
historical; the isolated R19 implementation starts at approved R18 `f1ba8f5`.
R19 source, ownership/fault controls and corrected ledger review are approved.
The [fresh R18/R19 screen](../ci-r18-r19-fresh-screen/README.md) measures
a 9.1899% known-cost reduction, with three p95 limits failing. R19 is only the
provisional cost anchor.

All retain existing memory/storage/API limits and unknown compute costs. No
acceptance or stopping conclusion follows. Verify this bundle with
`sha256sum --check SHA256SUMS`; [scope.json](scope.json) distinguishes historical
records from current status.

The [fresh-bootstrap audit](fresh-bootstrap-design.md) and [independent review](fresh-bootstrap-independent-design-review.md)
propose final schema columns and first eligible real-PUT inline ownership. The
conditional gross saving is $0.016; net benefit has a zero established lower
bound. This needs a substantive Native protocol, unchanged startup callbacks and
writes, actual GC-derived eligibility and paid fallback. The independent review
corrects the audit's incomplete three-row startup-read sketch: final columns
trigger another actual catalog reload. All such work must be measured and
charged. Source approval and real cursor/first-push gates remain pending; there
is no code GO. Original report bytes and gzip data identities are preserved in
[scope.json](scope.json). The separate joint-admission design below is the next implementation; bootstrap
remains deferred without a source pin.

The [joint-admission design](joint-chunk-design.md), [independent replay](joint-chunk-independent-design-review.md)
and [implementation preparation](joint-chunk-implementation-prep.md) remove only
the unallocated 15 MiB admission preference. All actual memory limits and pressure
evictions remain. The model avoids 193 further GETs ($0.06948), with a zero
unconditional net bound: larger resident sets can also lose later hits. R20 is
this isolated Joint policy; actual same-source controls precede source review and
a fresh priced screen. The bootstrap's earlier tentative R20 label never had code
or a source pin.

The [bootstrap implementation preparation](fresh-bootstrap-implementation-prep.md)
pins the approved R19 source and stages a cheap real startup/first-push proof
before broader implementation. Its conditional strict R19 cost margin is
$0.00307144563820; schema changes alone fall below the 1% target. Actual R20
source approval, measured costs and root implementation GO remain prerequisites.

The [response-retention audit](fit-response-design.md), [independent replay and review](fit-response-independent-design-review.md)
and [implementation preparation](fit-response-implementation-prep.md) propose
skipping optional upload response retention when it would evict existing cache
owners. Conditional savings are 70 GETs ($0.0252); a larger pressure stress
regresses three GETs, so the measured result remains open. The independent
review corrects the audit's ownership labels; production must use actual source
capacities. This candidate follows exact Joint source approval and root GO.
Bootstrap remains deferred. No implementation or benchmark is included here.
