# First mixed-flow attempt: rejected during warmup

No measured pair completed and no cost score is valid. The HTTP fetch delivered objects, but Git stored
loose objects with `fetch.unpackLimit=0`, even with `--keep`; the runner correctly rejected a zero retained
pack count. The client setting is corrected to `1` and covered by a real Rust HTTP regression for fresh
and ancestor-seeded clients. The failed raw trace and executable hashes remain here for review.

This attempt is not evidence of a hosting data-loss failure or a cost/performance improvement.
