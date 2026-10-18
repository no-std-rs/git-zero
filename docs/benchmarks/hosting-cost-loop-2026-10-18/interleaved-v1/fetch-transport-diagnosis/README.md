# Bounded large-response diagnosis

All 28 original-Worker NoExpect response cases passed exact counts in the
isolated Rust experiment. The 34,203,505-byte historical response size and the
9,440,303-byte verifier size did not reproduce their earlier stalls. No
production source changed; absence of reproduction supports no transport fix.

The [report](REPORT.md) and `results.json` correlate monotonic native HTTP stages
with client completion. A short Worker request log demonstrably precedes full
response delivery. The separate Expect control reproduces the existing local
gateway failure. Deleting the forwarded header in a temporary copy lets curl
fall back after waiting, but fails a strict client awaiting 100 Continue. That
workaround is incomplete and was not integrated.

`evidence.tar.gz` retains all selected Rust sources/drivers, instrumentation,
original and temporary Worker copies, commands, source hashes and concise raw
logs. Extract and run `sha256sum --check SHA256SUMS.publish`; this publication's
`SHA256SUMS` verifies its four files. No binaries, large synthetic fixtures or
emulator state are included. All owned services were stopped. The retained
138.5-second benchmark tail and verifier timeout remain valid failed evidence.
