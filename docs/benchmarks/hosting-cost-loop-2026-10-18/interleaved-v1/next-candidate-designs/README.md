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
is now independently approved; ledger review is complete and fresh measurement is declared.
It is absent from the completed R15–R17 screen.

The [multi-chunk audit](multi-chunk-next-design.md), [independent review](multi-chunk-independent-design-review.md)
and [implementation preparation](multi-chunk-implementation-prep.md) extend the
hypothesis to first-fit secondary admission under the same memory budget. The
modeled $0.12168 extra GET saving is conditional, with a zero unconditional net
lower bound. A hard 14 MiB response cap is rejected; eligible responses up to
16 MiB may reclaim chunks under actual joint pressure. Pruning requires trusted
current Full authority. The preparation's provisional source snapshot is
historical; the isolated R19 implementation starts at approved R18 `f1ba8f5`.
R19 source approval, ownership/fault controls and fresh measurement are pending.

All retain existing memory/storage/API limits and unknown compute costs. No
acceptance or stopping conclusion follows. Verify this bundle with
`sha256sum --check SHA256SUMS`; [scope.json](scope.json) distinguishes historical
records from current status.
