# Retaining failed benchmark stderr

The `hosting-bench` runner retains Native and reference `host.log` files after
an unsuccessful run. The exporter runs only in `main`'s existing error branch,
after `run` unwinds, failure snapshots have been taken and `Host::drop` has
stopped the child. It does not add work to a successful run or any workload
timer. Existing incomplete JSON, progress journal and retained fixtures remain.

Each failure writes a uniquely named `*.incomplete.failure-logs` directory next
to the incomplete report. The current CI workflow already archives that output
parent, so no workflow or provider-pin change is needed. Only expected trial
directories and their direct `host.log` files are inspected. Missing trial
directories were not started and are skipped. Missing logs in existing trial
directories have explicit manifest errors. Symlink/nonregular logs and trial
directories are refused. No fixture contents, request bodies, environment or
authentication dumps are exported.

The exporter streams the complete stopped file through SHA256 while retaining
at most its final 64 KiB. All retained log contents share a 512 KiB budget,
with the latest trial and both backends visited before older trials. At most
202 logs are considered, matching the runner's maximum 100 samples plus warmup.
The separately capped JSON manifest is at most 512 KiB; the existing 4 MiB CI
archive limit still fails closed. Larger original logs do not require larger
memory. There is no retry, body-cap, pricing, receipt or schema change.

`manifest.json` records each original path, byte count and full-file SHA256,
retained filename, byte count and SHA256, truncation and its budget reason, or
an explicit missing/read/write error kind. Missing counts/hashes remain null;
an existing empty log has its actual empty SHA256. Oversized displayed paths
retain the final 256 UTF-8 bytes and are explicitly marked truncated. Error
kinds cannot include response bodies or environment values. The exporter never
overwrites an existing evidence directory. Export failure is printed without
replacing the original benchmark error or changing its exit status.

This is an error-only amendment to the frozen common runner. It does not change
the Native providers or common Worker and does not diagnose or fix the earlier
transport failure. Run `39104016332`, attempt 1, remains original evidence at
source `82b64319bf9d39958c861b6fb7a9a6bae25fda9b`: its 64 MiB warm R14 excluded
warmup failed at `push_13` with HTTP 503 and an upload-retire ProxyWorker lost
connection before any R14 measured pair. That archive's SHA256 remains
`d69e7f3322fe41df87b083bcfeff874245af88cd566efb5c8360adc13ca33c8b`.
It omitted the fixture-host stderr and cannot be repaired after VM destruction.
No original samples are erased, replaced or reused by this diagnostic change.
The incomplete trial supports no cost/performance acceptance or stopping claim.
Final validation still requires five fresh original-R1/final pairs on all four
profiles, with every resource, accounting, content and latency guard reviewed.

Helper tests exercise binary tail ordering and complete hashes, the total
budget with newest-first retention, empty versus missing logs, Unicode path
bounds, maximum trial selection and symlink refusal. They use files only: no
Git workload, socket, service or benchmark flow is started.
