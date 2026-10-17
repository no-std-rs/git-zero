# R1 post-run latency diagnosis

All four five-pair profiles completed before this read-only log analysis. The Rust comparison is
`inconclusive`: known dollars fell 41.5336054851%, every resource guard passed, and 16 paired
Git-normalized latency guards failed. Original reports, tails and comparison are retained.

The following initial pushes are matched by repository namespace and first receive request to
`/tmp/git-zero-cost-loop-wrangler-r1.log`. Times are milliseconds. Nearby service requests are reported
in log completion order; the log has no per-request timestamps or parent/child tracing IDs.

| Profile / sample | Client wall | Advertisement GET | Receive POST | Manifest GET | Bundle PUT | Manifest POST | Native receive stage sum | Outer POST log line |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 MiB warm / 2 | 6,731.18 | 3 | 6,649 | 2 | 28 | 2 | 60.18 | 1452 |
| 4 MiB cold / 0 | 6,757.38 | 2 | 6,678 | 2 | 29 | 1 | 64.47 | 3535 |
| 4 MiB cold / 2 | 6,793.65 | 3 | 6,715 | 2 | 28 | 1 | 64.37 | 4857 |
| 4 MiB cold / 4 | 6,767.98 | 4 | 6,682 | 1 | 50 | 2 | 82.63 | 6179 |

Native stage sum is prepare + codec + storage + publication; it excludes HTTP/body handling, dispatch
and discovery. Each case kept the same DO instance epoch before and after the measured operation.
These cases localize the large delay inside the outer receive POST and outside the observed service
RPCs/native phases. Advertisement and measured R2/SQL RPC durations do not explain the missing seconds.
Existing records cannot distinguish Worker forwarding, Rust request-body handling, dispatch or scheduling.
No new instrumentation or service probe was introduced for this diagnosis.

A separate 4 MiB cold sample-2 append at log line 4917 took 7,772 ms in the outer receive POST and had
a nearby 7,721 ms bundle PUT at line 4912. That wait is localized to the R2 service bridge call and must
not be conflated with the initial-push cases above.

`latency-guard-failures.json` records all 16 failures with baseline/candidate paired-ratio medians and
p95 values. None were suppressed or counted toward the plateau stopping rule. Independent reruns remain
required before accepting R1 under the predefined latency limits. Cloud CPU and complete hosting costs
remain unknown.
