# Corrected local mixed-flow scalar screen

This is one complete **16 MiB cold** scalar-provider profile: two measured pairs
plus one warmup per backend. Every trial completed 32 interleaved pushes and 56
verified latest, historical and latest-recheck fetches. Each cloud trial performed
seven compactions. Initial uploads were 8,536,208 bytes; seven later uploads were
4,264,520–4,264,603 bytes each. Both measured cloud trials had identical actual
operation and byte counts.

This is not a four-profile cost score or a candidate comparison. The summary's
`whole_cost` is its whole-trace profile subtotal, not the fixed four-profile
objective. Worker CPU, DO duration, monthly SQLite storage and native compute
remain unknown. No production Cloudflare deployment ran.

The workstation was heavily contended and its load changed substantially during
the run. Cloud warmup took 516 seconds; measured cloud traces took 138 and 145
seconds. Git warmup took 100 seconds; measured Git traces took 46 and 98 seconds.
These backend durations include verification and observation outside request
timers. They do not establish a speed improvement or billed compute.

The local profile is retained separately from the isolated GitHub-runner
experiment. Its timings and machine identity cannot substitute for that
experiment's fresh matching baseline. The earlier failed zero-unpack-limit
warmup remains under `../../attempt-01-retention-failure`.

`RAW_SHA256SUMS` identifies the original uncompressed JSON; `SHA256SUMS` checks
the published files. Raw reports retain all operation rows and validation.
