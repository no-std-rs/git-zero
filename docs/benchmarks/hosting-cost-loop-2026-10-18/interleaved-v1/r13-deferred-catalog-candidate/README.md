# R13 deferred catalog correctness candidate

The [implementation ledger](implementation.json) identifies clean code d84c2f6
and ledger-only HEAD79011d7, all module hashes, final raw artifacts, prior failures
and exact accounting limits. This is correctness preparation, not a cost screen
or accepted performance result.

Eligible all-success retirement deletes packs immediately while retaining a
bounded durable debt marker. The following same-page upload can fold the
survivor catalog update into its existing write. Missing proofs use charged
recovery; arbitrary uncertainty requires full GC. A durable acknowledgment
before same-key reupload prevents old-proof replay. Older clients and exceptional
shapes retain eager updates. The supported >256-key scalar fallback remains
bounded and recoverable.

The [original review](independent-review-original.md) rejected the first freeze
for wedging that fallback. The [focused review](independent-review-correction.md)
verifies the correction and final ledger. Actual SDK Unsupported performs zero
I/O, then Repository fallback makes one CAS and 257 ordered real deletes;
interrupted/reopened recovery and subsequent mutations succeed.

Seven final actual Worker contracts, four final registered-Native fault cases,
103 library tests and Clippy pass. Earlier nine R12 controls retain their
separately identified intermediate sources. Normal final/reopened storage with
13 objects, 11 packs and 33 refs remains **24,576 bytes**. The 261-object fallback
stress fixture's larger database is explicitly separate.

In the bounded three-call control, actual SQL reads/writes fall from 9/4 to 8/3
with unchanged physical calls/bytes. The proposed four-profile saving is $0.028
gross per 1,000 replays before measured overhead; net saving is unknown.
All failed fixture assumptions and production findings remain in the ledger.

The [combined real-Git check](combined-validation.json) passes against the
integrated source with the capability correction: historical fetches with and
without haves, strict fsck/archive and a subsequent large push. Its 11.14 seconds
is a correctness-run duration, not a speed sample.

The correctness-evidence archive retains all ledger-hashed report/log files
at paths relative to /tmp; no fixtures, emulator state or executables.
Check publication files with sha256sum --check SHA256SUMS. Worker CPU, DO
duration, SQLite retention and native compute remain unknown. No acceptance
or plateau follows; a fresh screen and final five-pair run are pending.
