# Bounded catalog-page proposal

This independent audit reproduces all 704 fresh R8 operation rows and finds a
remaining representation change worth testing. Grouping up to 32 SHA-256/size
entries in one bounded SQL row could reduce ordinary three-key retirement from
three written rows to one: **$0.056 gross per 1,000 four-profile replays**, before
actual migration, lookup, spill and recovery costs. This is a projection.

The [design](design.md) preserves actual R2 operations, pre-CAS upload durability,
post-CAS deletion, peak witnesses and generation-checked recovery. It specifies
mixed legacy rows, bounded prefix routing, adversarial splits and deletion of
empty pages. Repeated churn must not leave an expanding routing structure.
The actual 24,576-byte SQLite guard remains decisive; another page fails it.

`projection.json.gz` retains source/report hashes, calculations and exact
bin-packing checks. The Python file only analyzes retained reports; execution
runners remain Rust. No source change, service or benchmark was run by this
audit. R9–R11 combined scores and this proposal remain unmeasured. No plateau or
global optimum is established; final five-pair validation is still required.
