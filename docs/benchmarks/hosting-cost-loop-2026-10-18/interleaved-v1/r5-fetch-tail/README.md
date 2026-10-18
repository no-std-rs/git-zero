# Retained R5 historical-fetch tail

The 64 MiB warm profile in [run 39084173778](https://github.com/no-std-rs/git-zero/actions/runs/39084173778)
contains a 138,537.578 ms `history_no_haves_04` fetch in sample 1. The same
operation takes 1,395.010 ms in sample 0. Both retain a 34,182,222-byte pack and
pass exact wanted-ref/tree verification, strict fsck and archive verification.
Neither measured row accesses R2 or reconstructs the native view.

| Measurement | Sample 0 | Sample 1 |
| --- | ---: | ---: |
| Client wall time, ms | 1,395.010 | 138,537.578 |
| Native upload codec, ms | 170.397 | 143.639 |
| Client user + system CPU, ms | 730 | 1,020 |
| Gateway / DO requests | 2 / 2 | 2 / 2 |
| R2 GETs / read bytes | 0 / 0 | 0 / 0 |

The corresponding sample-1 Worker log reports the upload POST as HTTP 200 in
146 ms. That log does not establish when its response body finished reaching
the client. These measurements localize work already accounted for; they do
not distinguish response transport/backpressure, client waiting, HTTP handling
or scheduling. There is no correlated TCP or native HTTP-write trace.

`localization.json` extracts the unchanged measured rows, relevant metrics,
original report/log hashes and log line. The original compressed profile is
retained in [the complete batch evidence](../ci-r2-r5-screen/README.md).
This tail remains in every comparison and invalidates a speed claim. Removing
cold discovery reads cannot by itself explain a warm fetch with zero R2 reads.
