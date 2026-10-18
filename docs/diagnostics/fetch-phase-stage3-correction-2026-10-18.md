# Stage-three correction of B1 and B2

This separate diagnostic correction starts from
`d2cab05869a7b932a910857fdd4e43bc02d79b7a`. The independent review at
`/tmp/git-zero-fetch-stage3-independent-review/REVIEW.md`, SHA-256
`1bb8465ebd8f78fbdb4aa06916085e4c23c3752000a8ff19faa95aad433ed4b1`,
blocked that source for an unsupervised resource helper (B1) and absent explicit
strict indexing of fetched packs (B2). Both findings are corrected here, with
one fresh tiny production proof and a targeted blocked-helper control. This
handoff is for independent correction review, not approval of large runtime.

All previous reviews, tiny01 receipts, d2cab metadata-only source/ELF archives
and manifests remain unchanged. Tiny01 did **not** perform explicit strict
index-pack validation. Its successful execution under its original checks does
not satisfy B2 retroactively. The earlier final metadata-only ELF remains
honestly unrerun. No prior proof claim or receipt was rewritten.

## B1: resource work belongs to the supervised case

The controller no longer runs `df` or aggregate capacity traversal before or
between cases. The old shared `Command::output()` helper is removed. Each
executor resource boundary invokes `df` through the existing command capture
path, inside the owned case PGID and inclusive work deadline, then reads memory
and walks the complete aggregate raw prefix there. Initial, fixture, operation
and final boundaries all cover the case's parent prefix, including prior
completed cases. The next case's initial check therefore includes cumulative
evidence from earlier cases; the controller does no between-case traversal.

Each boundary saves a started receipt before the helper, actual helper
status/stream hashes, and a completed capacity receipt before accepting or
rejecting its checks. Disk reserve, memory floor and full-prefix byte cap are
preserved. The two resource snapshots in the older cheap-supervisor CLI also
route through a supervised resource-only worker, avoiding another controller
escape. That legacy full control suite was not repeated here; normal resource
work is exercised by the fresh production proof, and blocking cleanup by the
targeted control.

Capacity remains a boundary sample, not a filesystem quota. Killable helpers,
aggregate traversal, stdout/stderr drains and joins run inside the case group.
The parent stays outside, uses the existing validated group watchdog and exact
owned-descendant reaping, and never joins a pending drainer. Timeout/failure
stops the batch without retry. A killed worker can lack a final case result;
the supervisor receipt and retained started/partial files remain authoritative,
with unavailable counters incomplete/null.

## B2: strict indexing of actual newly fetched packs

After the original timed gateway fetch and its after-observer, every newly
installed pack runs through `git index-pack --strict` inside the case group,
before existing want/tree, strict fsck and archive checks. It uses the actual
installed pack path, checks that the pack SHA-256 remains unchanged, and saves
an explicit validation receipt plus actual command/status/stdout/stderr hashes.
Failure stops. This is added offline local validation: the fetch command,
protocol v0, postBuffer 268435456, `--keep`, unpackLimit 1, local seeding and
Cloudflare observer order remain unchanged. It adds no service request.

## Failed control01 and the exact successful source

The first targeted control, at
`/tmp/git-zero-fetch-stage3-blocked-preflight-20261018-01`, expired before
reaching the helper. The resource-only worker unnecessarily ran the full
Native/Node/workerd provenance audit first; it was observed in D state during
the two-second work budget. The watchdog TERM-reaped the worker in 2217 ms,
with complete outer drains and no remaining group. No helper, listener,
resource result or final case result existed. This is retained as a failed
control, not a successful blocked-helper proof.

Before rebuilding, that source and actual ELF were preserved independently at
`/tmp/git-zero-fetch-diagnostics-immutable/stage3-correction-tested-v1`;
map SHA-256
`43c553132a4100b69ccc1eef33f4524684277eceea8647dbc1e702cf5c61815f`,
ELF `f62bf5c015a27fb7b3a652a5b46530f960c902d38854d67be5c484ac688f12a2`.
The disclosed correction removed only that unrelated full audit from the
resource-only worker, so its first work is the actual capacity/helper path.
Production before/after provenance audits and all deadlines remain unchanged.
The invocation wrapper pins the two Rust sources and actual ELF before each
control/run. No production tiny run occurred on this failed-control version.

The successful targeted control02 and the single fresh tiny02 production proof
used exactly these frozen SHA-256 identities:

| Item | Actual tested identity |
| --- | --- |
| Supervisor source | ae6489863dd71259a5050d7c01b1cf4e8909c883a2d762f86a0b87fe9bdf6c48 |
| Executor module | 8d8a1f8de582c1f77fbc905cdbd7563a2b49f816171a228f1cd1e6004750341c |
| Actual supervisor ELF | b36f7564ca6b83f4d316f617b0eaae728028296caf3f9020f4fac315362ad182 |

Before those runs, all then-tracked source and the actual ELF were copied to
the read-only independent archive
`/tmp/git-zero-fetch-diagnostics-immutable/stage3-correction-tested-v2`.
Its 689-entry map SHA-256 is
`895d7be639e38ff79674e6de186e5ece3a322fc43f19c1092950f31141d8082a`;
unchanged Native/stage-one runner ELFs use existing independent immutable
aliases. No Rust source edit or rebuild followed either successful gate.
The final clean source archive/commit receipt is in the external handoff.
Historical mutable paths resolve through their exact archive maps.

## Successful bounded evidence

`/tmp/git-zero-fetch-stage3-blocked-preflight-20261018-02/proof.json` passes.
The actual resource command path substitutes only a fixed proof helper for
`df`. Helper PID 1454688 inherited case PGID 1454685, ignored TERM and blocked
while owning port 8853. At the unchanged two-second deadline, the controller
terminated the group, adopted the helper, KILLed and reaped it, completing in
2462 ms within the separate fixed five-second cleanup grace. No owned group
or listener remained. Started resource/command receipts and the helper's
stdout prefix survive; completed resource/counter receipts remain absent/null.

`/tmp/git-zero-fetch-stage3-tiny-production-20261018-02/proof.json` passes on
the same source and actual ELF: 85,529 ms diagnostic elapsed, 120-second work
budget plus fixed five-second grace, leader exit 0, completed outer drains,
no outer timeout or TERM/KILL and no remaining group. This elapsed value is
not a benchmark or comparison with tiny01.

The unchanged separately sealed tiny plan has three own generations and
32,790 working-tree bytes, nine operations and the same three independently
sealed wants/trees as tiny01. All wants/trees were verified before services
started. Three pushes completed; all six reads passed nonempty-pack, explicit
strict index-pack, want/tree, strict fsck and full archive-hash checks. All six
strict-index command completions exited zero and retain actual pack and stream
hash receipts. All 104 captured commands succeeded in case PGID 1457548;
15 were actual resource helpers, with 15 full-prefix boundary receipts.

Live before/after maps cover Native, Node/Wrangler/workerd and descendants in
that PGID. Actual Native PID 1461520 was environment-eligible, with only the
four canonical Git settings plus `GIT_ZERO_PHASE_LEDGER`; environment values
are not logged, and Native receives no Trace2. Complete source/runtime
provenance before and after agrees. All 12 total-versus-phase counter checks
close startup, ordinary work and observer/cleanup scope. Native was reaped
after TERM and Node after exit 143, with complete streams; ports are free.

The Native ledger contains 127 complete records and 61 actual request-hash
records, with 10,030,457 ns boot-clock uncertainty. Applicable v0 receipts show
one advertisement certificate, two shortcuts, 18 witness hits, two consumed
read preparations of six plans, two history projections and 176 lineage bytes.
Receive consumption remains zero of three plans, all unsupported. Native,
child and Worker CPU, DO duration and full bill stay null. Local emulator
counter closure is not a cloud bill or a performance claim.

Initial capacity recorded 8,569,131,008 disk bytes and 40,672,022,528 memory
bytes available. The final boundary recorded 1,702,794 aggregate logical bytes
under the 16-MiB tiny cap, 10,194,923,520 disk bytes and 57,289,621,504 memory
bytes available. External host disk/memory changes are not attributed to this
tiny case. All build/run preflights exceeded the two-GiB disk reserve.

## Unchanged limits and next boundary

Native/hooks, Worker, stage-one runner, dependencies/lock, benchmark fixture,
schedule, scorer, rates and production cache/transport/retry/timeout policies
remain exact. Native actual ELF remains
`3ccf43bb5cca872294191fef57df9d4469ddd5dad41e9b5abf4cd6e270d1f060`;
stage-one runner remains
`d2e21856771f7fb3b8bb0e15517c4ab150645dc623255880d90699663a1db943`.
There is no new dependency or unsafe code. The existing Linux /proc,
PID-generation, scheduling and uninterruptible-kernel/filesystem limitations
remain: these are practical watchdog bounds, not hard-real-time guarantees.

Neither sealed large fixture was generated or replayed. Full 32-generation
reconstruction and large runtime/request/pack evidence stay null. The fixed
two plans, maximum two replicas each/four cases, 1200-second inclusive case
work plus separate five-second cleanup grace, eight-GiB aggregate raw cap and
memory proposal, stop-without-retry rule and R19-versus-historical-R18
distinction remain unchanged. No new tmpfs/raw prefix is authorized or used.
Mandatory final-five work remains separate and was not run.

The external handoff directory
`/tmp/git-zero-fetch-stage3-correction-handoff-20261018` retains failed and
successful control history, both tested source/ELF versions, tiny02, static
checks, scoped manifests and the final source/archive receipt. Stop for fresh
independent correction review and separate root runtime/resource GO.
