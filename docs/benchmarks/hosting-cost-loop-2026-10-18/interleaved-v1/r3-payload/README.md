# Payload batching candidate

Clean implementation `e4a2085d4b5c8ec3f19082776a11e65d15972bee` was integrated and
pushed as `d765e99`. Two full 8 MiB chunks can share one bridge request. Payload
stays capped at 16 MiB; the frame ceiling rises by 2,060 bytes. Chunk storage,
actual R2 reads, snapshot limits and retained cache budgets stay unchanged.

The correctness record covers package tests, strict Clippy, seven real workerd
contracts and process restart. The full-payload contract observes one Worker
request, one DO request, two actual R2 GETs and 16 MiB of read bytes, including
equal repeated keys. Exceeding the payload cap fails before binding I/O.

Cost, latency and total runtime memory remain unmeasured for this candidate.
Its next screen compares fresh R2 baseline `e6f4ad0` with this implementation
on the same VM per profile, using the original scalar Git reference.
