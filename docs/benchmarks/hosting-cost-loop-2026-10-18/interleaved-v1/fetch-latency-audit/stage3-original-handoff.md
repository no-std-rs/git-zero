# Stage-three frozen handoff

Clean SOURCE `d2cab05869a7b932a910857fdd4e43bc02d79b7a` on `perf/hosting-fetch-diagnostics`, parent
`c964a967ccebf76aa1137412e158630bbe668937`, worktree
`/tmp/git-zero-hosting-fetch-diagnostics`. Exactly four changed files: supervisor,
new Rust executor, stage-three Markdown and JSON. Native/hooks/Worker, stage-one
runner, dependencies/lock, benchmark/generator/schedule/scorer/rates and policies
remain unchanged.

One tiny same-path production proof passed:
`/tmp/git-zero-fetch-stage3-tiny-production-20261018-01/proof.json`
(SHA-256 `5dbdfc0c65584e6435384961261349337b3f70b8d66cf12743b746cf049c32f0`).
Three own sealed generations (32,790 working-tree bytes), nine real Git v0
push/read operations, all strict want/tree/fsck/archive/nonempty-pack checks,
actual eligible Native environment, live PGID inheritance, complete 12-counter
startup/ordinary/observer scope closure and owned cleanup. Diagnostic elapsed
37,440 ms. Ledger has 127 complete records and 61 request-hash records. Native,
child and Worker CPU, DO duration and full bill remain NULL. Tiny wiring proof
is not a large latency-tail reproduction or performance claim.

Actual tiny-tested supervisor ELF:
`c3d142bb55a2cad1f1115ab0be401c0556bd57952b2da428adc134de16c73cf7`.
Its source and all three actual ELFs were archived before any rebuild at
`/tmp/git-zero-fetch-diagnostics-immutable/stage3-tiny01-tested-v1`, map SHA-256
`0d0d13a92d32ce1cf317a2efdd549994c37fb82d002ca76f19132c89531cc7a1`.
Original mutable target/source paths in historical receipts are historical;
use immutable aliases to verify their tested identities.

Final metadata-only unrerun supervisor ELF:
`7e5db2316b8cc7def45805cee3c2cb4373475a47a95bc61577546802d0b5ee4b`.
Only two JSON receipt assignments changed after tiny/control proof: inherited
fixture-size zero becomes NULL and scope explicitly says capacity-only. No
execution/check/order/ownership/workload change. Exact diff:
`tested-to-final-metadata-only.diff`, SHA-256
`12130a8def209d111ddf35860847d749f2ce54e0777c491891a842a8c72b4796`. Final build, format and diff checks pass;
there was no second production tiny run or post-correction runtime control.
Final clean source plus actual supervisor ELF archive:
`/tmp/git-zero-fetch-diagnostics-immutable/stage3-d2cab05869a7b932a910857fdd4e43bc02d79b7a-metadata-only-v1`,
map SHA-256 `00af983c79bd6334c892e0c677ddaf4a7ae90794ddd900b4c0b832db95abf5f6`.
It contains all 686 clean tracked-source copies, the copied final supervisor
ELF, and references to already independent immutable unchanged Native/runner
ELFs (689 entries). Native actual ELF remains
`3ccf43bb5cca872294191fef57df9d4469ddd5dad41e9b5abf4cd6e270d1f060`;
stage-one runner remains
`d2e21856771f7fb3b8bb0e15517c4ab150645dc623255880d90699663a1db943`.

Changed supervisor controls passed before the metadata correction at
`/tmp/git-zero-fetch-stage3-supervisor-controls-20261018-01`: normal113ms,
blocked socket2385ms, orphan-held pipe2368ms, injected error cleanup. All owned
groups and listeners removed. Negative batch authorization failed before prefix
creation. `final-integrity-check.json` verifies all575 proof files and prior
manifests/reviews unchanged; final read-only /proc checks found no owned group
or8853/8854listener. No socket/runtime control was repeated after correction.

Future large executor supports only the two fixed sealed plans, maximum two
replicas each/four cases,1200s case work plus fixed5s watchdog cleanup grace.
It verifies all32 sealed wants/trees before production services and replays the
full original push/read/observer prefix. Failed observer/timeout stops without
retry. Large fixture reconstruction stays NULL: no large object generation or
replay occurred. Current diagnostic R19 provider differs from historical R18.
Separate exact-source/runtime GO and an approved fresh owned8GiB raw prefix
remain required. No tmpfs prefix has been authorized or used. CPU stays unknown;
Linux scheduling/uninterruptible syscalls prevent a hard-real-time guarantee.
Capacity caps are checked at boundaries, not enforced as filesystem quotas.

Remaining freeze-write preflight observed 7859728384 free disk bytes, above2GiB.
Host disk fluctuated externally; root's inactive incremental-cache cleanup did
not touch this target, source, actual ELFs or evidence. No build/runtime followed
the earlier low-disk observation. R20/main state was not touched.

STOP for fresh independent source/executor review. No large runtime GO.
