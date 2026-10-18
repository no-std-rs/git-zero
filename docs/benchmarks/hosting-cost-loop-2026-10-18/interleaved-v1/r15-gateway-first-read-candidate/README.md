# R15 gateway first-read candidate

[Protocol](../../../../hosting-gateway-first-read.md) and
[implementation ledger](implementation.json) describe the reviewed source
`5a7f74f58336b5474dc9920cd20857ab8e573910`.

The final-source real-Git on/off control saves one Worker and one DO callback
per activated cold fetch. Its fetch phase changes 24→15 requests of each kind,
with identical 30 GETs, 104,943,319 bytes, 13 SQL reads and five response bodies.

Local gates pass 23 fault modes, 13 header controls, four legacy/native
contracts and 113 library tests. Realistic reopened SQLite remains 24,576 bytes.
The ledger labels older source controls separately and records the narrow
failed-response Native byte-counter omission being corrected by R16.

[Independent continuity review](independent-ledger-review.md) verifies all
2,330 artifact records and 2,122 capsule members. Large reproducible payloads
and binaries retain hashes/sizes; the capsule contains source maps, receipts,
traces and logs. Verify publication files with `sha256sum --check SHA256SUMS`.

This is correctness evidence. Fresh cost/performance screening and five fresh
original-R1/final pairs are still required. Full billing remains unknown.
