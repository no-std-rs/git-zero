# Prepared final-upload publication candidate

R9 is integrated as `8af8c48fb272624bb67a0cb3923fad154c3a6dcf` from clean
implementation `fc168707cc43784dfd43f374457436c84dd14865`. It stages one final
new chunk and negotiates bounded upload/CAS/retirement. Actual upload/catalog
sync precedes manifest CAS/sync, then real retirement. Bounds and pre-I/O fallback
preserve eager APIs; ambiguous replies require authoritative recovery.

The focused real-service contract saves one Worker and DO callback with identical
R2 PUT/delete counts, written bytes and SQL rows. All 32 R5 pushes have an eligible
final tail, projecting 32 callbacks/profile or $0.0576 across 1,000 repetitions
of four profiles. This is an unmeasured projection, not a candidate score.

The [clean implementation record](../../../hosting-publish-upload-validation-2026-10-18.md)
covers 87 package checks, 16 production Worker contracts, two fault contracts and
four actual process-restart phases. Independent combined verification at
`457e2fa97b10cf57e47eea7e1c0fcb849d90780b` passed 98 package tests, strict Clippy
and formatting, four real production contracts and two strengthened Rust
contracts. These verify R8 discovery certificates alongside fused new-pack
publication, uncached fetch contents, strict fsck/archive and an actual cold
listening CLI restart. Worker SHA-256 is
`83b52ba0eabd9f752aa1c243ce285f20ab78caac9244329d00d0db788306f9bc`.

`combined-validation.txt` retains exact commands, hashes and unsuccessful
attempts. Sandbox bind failures were retried unchanged with loopback access.
The known local gateway Expect/100-continue failure was retained; fixed workload
requests use no Expect header. A NoExpect CLI transfer also timed out at 20 s
with 7,133,184 of 9,440,303 bytes; the same semantic contract passed with a 90 s
transport deadline. This is not a transport fix or a benchmark sample.

`combined-contract.rs` is the temporary Rust correctness driver, compiled as
recorded; no MJS runner was introduced. Owned services were stopped. Candidate
benchmarking, cost/resource/latency guards and final five-pair validation remain
required. Worker CPU, DO duration, monthly SQLite storage and native compute
remain unknown; there is no speed, isolate-fit or full-bill claim.
