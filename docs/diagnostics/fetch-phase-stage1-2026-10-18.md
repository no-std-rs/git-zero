# Stage-one fetch phase diagnostics

This is diagnostic source based exactly on approved R19 commit
`f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5`. It changes no benchmark,
scoring, rate, workload, provider pin, Worker module, dependency, lockfile,
timeout, retry, or cache policy. No production latency fix or cost/speed claim
is established. Large retained-case replay is not implemented by this runner
and requires independent source/gate review and separate root GO.

## Opt-in observations

Set `GIT_ZERO_PHASE_LEDGER` to a writable JSONL path for a Rust process.
Without it, no ledger file is created. Logs contain request path (without
query), method, body length/hash, bounded stage timestamps, counters and
error kind/OS code. They omit request headers, tokens, payloads, process
arguments and error-message content. No public correlation header is added.
Native requests get process-local ordered IDs; callbacks, Git children and
view records inherit that request ID.

Each HTTP request records accept, headers/body complete, handler begin/end,
response header/body writes, connection close and write errors. View prepare
and materialization have their own enclosing records. Git records mark the
actual spawn and the observed exit/output collection; the latter includes
waiting/scheduling and output draining, not a precise kernel exit timestamp
or CPU measurement. `native_read` also records the logical request body hash.

HTTP callbacks record connect, request write, status/header read, body read,
validation, status, successful wire bytes and precise failed stage. Buffered
reads can read body bytes during header processing; wire counts reflect
socket reads, not a claim that all those bytes were already decoded. WS
callbacks record connect/upgrade, send, header, first body segment, terminal,
ACK, logical body lengths and completed frame/message counts. These are not
WS wire-byte or CPU measurements. Application success can coexist with a
failed transport-only ACK, retaining the original behavior.

The ledger emits one row per operation, never one per chunk/read/file.
Each process bounds its ledger to 16 MiB and 24 stages per row, with a
truncation marker. Failure to open/write the ledger means missing evidence,
not zero work. The runner drains child stdout/stderr concurrently, retaining
up to 2 MiB each before checking exit success, and bounds each owned runtime
log to 2 MiB while continuing to drain. It has a 180-second command budget,
90-second client-child bound and bounded readiness/cleanup waits. It owns
and terminates its process groups, reaps leaders and verifies no live group
members remain; application ports are only 8853 and 8854.

Stage timestamps use `Instant`. Cross-process placement uses the Linux boot
ID plus `/proc/uptime` calibration with explicit at-least-10-ms uncertainty.
This is an approximate common boot-clock anchor. All Native/child/Worker CPU,
DO duration and full-bill fields remain null. Instrumentation overhead and
runtime/file-cache effects prohibit performance claims from these controls.

## Final cheap proof

Final evidence is `/tmp/git-zero-fetch-phase-stage1-proof-20261018-03`.
`proof.json` passed; `source-runtime-before.json` and
`source-runtime-after.json` match each other and the frozen source files.
They seal the actual runner, Native binary, Git 2.53.0, Node v24.10.0,
Wrangler 4.149.0 and workerd executable. Owned runtime process snapshots also
capture the actual Node/workerd/Native executable hashes before and after.
`inputs.sha256` seals all retained proof files.

The Rust runner creates independent tiny three-generation fixtures (64 KiB
per added file) and runs disabled/enabled logging cohorts sequentially
through the unchanged production Worker gateway. Its real Git historical
fetch and separately labeled exact saved pkt-line Content-Length/chunked
POST controls pass strict index-pack, strict fsck, want/tree and archive
checks. The two framing controls return byte-identical 65,777-byte packs in
both cohorts. Sealed historical want is
`750324febf218b5ea75fd6a0c5c6618717782e23`; tree is
`bc3799289611fbcbc176cc0959f388c1403bf2aa`. Exact saved request length is
109 bytes and SHA-256 is
`01aefe3f49b7f70352c338de51c74ba8c44c3d33f1beb87c38534a3362c83298`.

Both cohorts report nonzero supported history projection (1), retained
lineage bytes (176), and response cache hit (1). The shell-injected
`GIT_EDITOR` was unset for both final cohorts. The traced Native's actual
`/proc` environment contains only allowed `GIT_PAGER` and
`GIT_ZERO_PHASE_LEDGER`; its Rust-generated environment receipt records
names and eligibility, omitting values. The disabled Native had exited
before that external receipt collector attached: its actual `/proc` receipt
is **missing**, not reconstructed. Identical spawning code/environment and
its nonzero lineage/projection metrics substantiate only this narrow gate.
The receipt collector source and executable hashes are in validation JSON.

The real Git client uses its default v2 discovery. Advertisement certificates
and `gateway_read_consumed` are zero: the proof does not cover v0 certificate
or successful gateway-first-read continuation optimization. Later latency
cases must preserve their original sealed v0/capability/body/fixture semantics
and require applicable supported fast-path counters.

After a complete pre-failure metrics receipt, the runner intentionally stops
only its Worker and requests Native `/metrics`. It retains actual 503 headers
and the actual 17-byte body before interpreting status. The traced callback
records `GET metrics`, `connect_begin`, `ConnectionRefused`, OS code 111.
After-failure counters are null/incomplete. This synthetic connect-refused
failure does not identify the old R20 EAGAIN cause or reconstruct its missing
body. All four final cohort groups stop and are reaped without forced KILL;
8853/8854 are free. Missing billing measurements remain null.

Final ledger counts: 19 HTTP requests, 9 service callbacks, 8 Git children,
8 materializations, 4 native reads, 3 fetch-view preparations, 4 service-socket
operations. Four WS operations prove terminal and ACK observation. Four
completed fetch HTTP records pass phase-order/body-hash checks.

## Validation and retained failed controls

- All 132 existing host library tests passed with normal local-socket access,
  two test threads, and `GIT_EDITOR` unset (67.09 seconds). The original
  sandbox attempt failed on denied loopback binds; the first socket-enabled
  attempt passed 120/132 because inherited `GIT_EDITOR` deliberately disabled
  existing advertisement identity/lineage. No production policy was changed.
- Final framing tests pass for partial Content-Length/chunked input, existing
  Native 100-continue, premature EOF and ambiguous framing. Ledger-enabled
  header/body disconnect controls prove `BrokenPipe` at the original failed
  write stage with exactly 17/110 bytes written, even after connection close
  is recorded. The final real gateway proof passes after this ordering fix.
- Proof 01 remains failed and retained. An unnecessary forced-Expect gateway
  control hit the known unsupported interim-response path: actual 500 headers,
  278-byte body and Workerd errors are saved. Only that extra runtime control
  was removed; no Expect stripping, production workaround or echo sweep was
  applied. Real Git and CL/chunked controls were unchanged.
- Proof 02 remains an earlier passing boundary/equivalence control with the
  unsupported inherited editor environment. It is not optimized-path evidence.

Build uses only `/tmp/git-zero-fetch-diagnostics-target-j2` and cargo `-j2`.
No tmpfs, large fixture, deployment, CI, push, merge, shadow work or API work
was performed. Final artifacts and the isolated target occupy under 1 GiB;
disk headroom remained above the 2 GiB floor.

To reproduce only this cheap gate, use a new output directory and the same
reviewed runtime path (local socket access must be permitted):

```sh
CARGO_TARGET_DIR=/tmp/git-zero-fetch-diagnostics-target-j2 \
  cargo build -j2 -p git-zero-host --bin repository-host --bin fetch-phase-diagnostic

env -u GIT_EDITOR \
  GIT_ZERO_PHASE_LEDGER=/tmp/NEW-fetch-phase-proof/client-phase.jsonl \
  /tmp/git-zero-fetch-diagnostics-target-j2/debug/fetch-phase-diagnostic \
  --cheap-proof /tmp/NEW-fetch-phase-proof \
  --wrangler /tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js
```

Unknown inherited `GIT_*` variables must also be absent. Client Trace2/curl
logging is injected only into the external Git client command, never Native's
parent environment. No replay is authorized by this reproduction command.
