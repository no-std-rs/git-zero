# Reviewed next-candidate designs

This bundle separates actual R13 receipt replay from unmeasured proposals.

R14 atomic metadata and R15 first-fetch callback implementations have local
correctness approval; their fresh screen is pending. R16 prepares a Full view
for a small receive once. R17 proposes bounded socket service RPCs. Neither
proposal's request-count arithmetic is a measured net saving.

The fresh R13 replay reproduces all 704 measured rows and its known fixed
$1.80461843618 objective. R16's 48 structural callbacks require fresh classifier
and receive controls. R17's post-R15/R16 cohort is conditional; every actual
Upgrade, fallback and recovery must be counted.

Each design has an independent review and implementation preparation. All
retain existing memory/storage/API limits and unknown compute costs. These
records establish no speed win, global minimum or stopping result. Verify
publication files with `sha256sum --check SHA256SUMS`.

The protected-chunk design, independent review and implementation preparation are prospective evidence for one bounded immutable GET cache. Its candidate source is excluded from the R15–R17 screen. Conditional GET counts and cache-survival arithmetic are hypotheses; only a later fresh screen can establish net cost.

The multi-chunk audit extends that hypothesis to bounded secondary admission. Its modeled GET savings require independent design review, real ownership/pressure gates and fresh measurement; the current screen enables neither chunk-cache design.
