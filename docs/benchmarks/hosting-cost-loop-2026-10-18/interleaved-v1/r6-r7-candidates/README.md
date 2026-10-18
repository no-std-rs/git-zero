# Prepared retirement and verification candidates

R6 fused retirement is integrated as `e4cdb32e92982d5f56a0ab88fe8648d5bac33b05`
(clean implementation `872bff3`). R7 single chunk verification is integrated as
`50e2fd8d46473c9dfdec2eb693c2c92fdfeeaeb4` (clean implementation `50cba90`).
Only an additive Rust import conflicted during integration; both capabilities
were preserved and independently checked together.

R6 negotiates a bounded publication/retirement operation. Durable manifest CAS
still precedes actual per-key deletes, and complete debt survives lost replies.
Its real three-key contract observes one Worker/DO callback rather than four,
with the same actual R2 deletions and catalog SQL. The published R2 traces have
21 retired keys per profile, projecting $0.0378 less known cost across four
profiles repeated 1,000 times. This remains unmeasured for R6.

R7 carries immutable owned verification proofs through both service wrappers.
HTTP batches and scalar fallbacks retain length/hash checks; reconstruction
consumes an exact ordered metadata match before writing any native file. This
removes a duplicate native SHA-256 pass without changing priced operations,
framing, retained caches or storage bytes. It cannot explain or fix the observed
warm transport/client stalls.

Independent integration validation passed 80 package tests, strict all-target
Clippy, formatting, and nine actual workerd contracts. These include registered
push/fetch, compaction and three-key retirement, HMAC/cache invalidation,
typed 16 MiB reads, corruption rejection and codec restart rehydration. There
were no failures or retries. The owned runtime was stopped after verification.
`combined-validation.txt` records the frozen source, commands and outcomes.

The next declared fresh two-pair batch compares R5, R6, R7 and the separately
reviewed R8 discovery candidate after review of the preceding R2–R5 screen.
Intermediate suites are shared by adjacent comparisons, so results are correlated. There are no benchmark results for these
candidates yet. Final validation still requires five fresh pairs of original
R1 versus the selected final candidate. No speed win, formal acceptance,
plateau or complete cloud bill is established here.
