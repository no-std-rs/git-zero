# Prepared historical-read and empty-probe candidate

R10 integrates clean source `9189176` plus identity fix `45018ba` as `5bec5ec`
and `e30e9c324fe12e9bbbd86959b1710cc9ea5b270e`. The tested clean commit and
integration have identical repository-host source/test trees. No Worker,
benchmark, scorer, pricing, or objective change is part of this candidate.

Private ephemeral lineage selects complete historical pack closures for narrow
cold v0 requests. Full authoritative metadata is validated before selected I/O;
all selected bytes pass ordered length/hash checks before native files appear.
External changes and ambiguous publication discard proof. Unsupported requests,
real pushes and global inventory restore Full. A cached latest recheck can leave
a partial view; its required final full inventory restore is counted explicitly.
All response/cache, advertisement and lineage owning allocations share the
existing 32 MiB budget, including pending metadata. Warm mode remains Full.

An exact successful empty v0 receive probe (`0000`) needs no object processing.
The shortcut retains authority and full metadata checks, requires matching
codec-policy certificates, preserves scope/cache/lineage and returns the exact
native empty response. Ordinary nonempty and ref-only receives still use strict
native preparation. Native Git 2.53 source review and byte/header regressions
cover unborn/populated/partial cases. This removes a full reconstruction from
actual default-client large pushes. The fixed benchmark's 256 MiB post buffer
avoids that probe, so this off-trace benefit is not added to projected dollars.

Independent source review found no remaining blockers at `45018ba`. Final exact
CI-environment checks passed 110 tests, strict Clippy, formatting and diff checks.
The fresh registered production Worker Rust driver passed historical fetches
with/without haves, latest cache hits, lightweight metrics, explicit inventory
restoration, empty probes, a real 10 MiB push, strict fsck and archive equality.
The first inventory restore charged five GETs / 9,448,326 bytes / one callback;
the final restore charged six GETs / 19,938,799 bytes / two callbacks. The empty
probe read no R2 payload and kept partial scope; its real push restored Full once.
Read phases kept R2 writes/live/peak and SQL writes stable; SQLite stayed 20,480 B.

`validation.txt` and `contract.json.gz` retain exact receipts, source and checks.
Earlier sandbox failures, failed eligibility assertions, fixture corrections and
a 35.854-second local publication tail remain recorded. Owned services stopped.
These are correctness checks, not performance trials. A fresh unchanged screen
and final five-pair original-R1 comparison remain required; no speed, cost
acceptance, formal plateau or complete cloud-bill claim is established.
