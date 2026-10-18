# Stage-three executor and one tiny production proof

The isolated diagnostic branch adds a Rust replay executor to the approved
stage-two supervisor, based on `c964a967ccebf76aa1137412e158630bbe668937`.
One separately sealed tiny production proof passed. Neither historical large
fixture has been generated or replayed, and their reconstruction status remains
null. Stop here for independent source/executor review and separate runtime GO.
There is no production latency fix, benchmark sample, cost improvement or full
bill claim.

The approved Native/hooks, stage-one runner, six Worker modules, dependency and
lock files, hosting-bench fixture/schedule, scorer, rates, cache and transport
policies remain unchanged. Native is R19 source
`f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5` plus approved stage-one hooks
(`a827df032e87374737b1ac7e36fb518b507e8411`), distinct from historical R18.

## Tested build and final metadata-only build

The tiny proof and subsequent supervisor controls used these exact SHA-256
identities:

| Item | Tiny-tested identity |
| --- | --- |
| Supervisor source | b8612ce2b39449dd6fa49c196360c8410e9008507b696d47bf95bc3cb86bf425 |
| Executor module | bdb69e34f1399fc0eef4ad4393bc88afd3fa0464392df60681d63274a8ca5076 |
| Actual supervisor ELF | c3d142bb55a2cad1f1115ab0be401c0556bd57952b2da428adc134de16c73cf7 |
| Actual Native ELF | 3ccf43bb5cca872294191fef57df9d4469ddd5dad41e9b5abf4cd6e270d1f060 |
| Actual stage-one runner ELF | d2e21856771f7fb3b8bb0e15517c4ab150645dc623255880d90699663a1db943 |

Before any rebuild, independent read-only copies of all 684 then-present source
files and three actual ELFs were archived at
`/tmp/git-zero-fetch-diagnostics-immutable/stage3-tiny01-tested-v1`.
Its 687-entry relocation map SHA-256 is
`0d0d13a92d32ce1cf317a2efdd549994c37fb82d002ca76f19132c89531cc7a1`.
Original mutable paths in historical receipts must resolve through that map;
they do not describe the current contents of reusable target paths.

Root then approved exactly two metadata assignments in `capacity()`: replace
the inherited cheap-proof `large_fixture_bytes: 0` with null, and label that
receipt as capacity-only, without a fixture-size measurement. The supervisor
source remains byte-identical. The final executor SHA-256 is
`08630d52326e76941c210b1474608bc01579a0e606bf7cb4ef656be6505d1528`;
the rebuilt actual supervisor ELF is
`7e5db2316b8cc7def45805cee3c2cb4373475a47a95bc61577546802d0b5ee4b`.
This final metadata-only build has **not** been rerun through production or
supervisor runtime controls. All tiny01 receipts remain unchanged, including
their earlier inherited resource field. Source comparison proves no execution,
scheduling, framing, ownership, workload or resource-check change. The exact
two-line diff is in the handoff directory, SHA-256
`12130a8def209d111ddf35860847d749f2ce54e0777c491891a842a8c72b4796`.
Final format and supervisor build checks pass. The final clean source/ELF
archive and commit receipt are recorded separately in the external handoff.

The complete approved stage-two source and all three actual ELFs were also
archived before stage-three edits at
`/tmp/git-zero-fetch-diagnostics-immutable/stage2-c964a967ccebf76aa1137412e158630bbe668937-complete-v1`;
its relocation map SHA-256 is
`84a6771d77dadb3fd8f94a7c271372ab8df9afd48797ded821ddb8c8a86ef083`.
Earlier proofs, reviews, manifests and archives remain unchanged.

## Executor boundaries

The controller starts a fresh case PGID and stays outside it. The executor,
Native, Node/Wrangler/workerd, Git and HTTP/drainer work inherit that PGID.
Only the supervisor establishes a process group; the executor does not use
`setsid`, detached processes or process-group resets. Actual live descendant
maps and executable hashes are saved before and after the operation prefix.
Native's `/proc/PID/environ` is captured while alive, retaining Git variable
names and eligibility rather than environment values. Unknown inherited Git
variables are removed. Native receives no Trace2 setting.

The existing Linux supervisor bounds work, signals only validated owned groups,
adopts/reaps exact owned descendants and preserves partial streams on failure.
Successful executor exit with remaining descendants/drains begins cleanup
immediately. Ordinary service unregister, final observer, TERM/wait and stream
joins are inside the case work deadline. A timeout or failed observer stops the
batch without retry. Missing or timed-out counters remain incomplete/null.
The generic watchdog's after-timeout counter field stays null even for a
successful case; successful scope closure has its own complete receipt.

Future large cases have 1200 seconds of work, including fixture construction,
prefix, client Git, observations, socket/drain waits and ordinary cleanup, plus
a separate fixed five-second watchdog cleanup grace. At most two replicas of
each fixed plan are accepted, four cases total. The tiny proof used 120 seconds
plus five seconds. These are practical Linux watchdog bounds: scheduling and
uninterruptible controller-side `/proc`/filesystem syscalls can delay polling or
receipt writes. They are not hard real-time guarantees. Linux `/proc`
visibility, unchanged PID generations and inherited process groups remain
prerequisites. No new unsafe code or dependency was added to stage two's
approved supervisor FFI.

Commands retain 128 KiB per stdout/stderr stream while hashing/draining the
entire stream. Started receipts precede waits; completed receipts include actual
exit, byte count, hash and PGID. Rust HTTP observations preserve actual status,
headers and bounded body before rejecting failures. They retain the frozen
ten-second socket timeout and impose an eight-MiB observation cap. No fault was
injected into the tiny production run; resistant-child/error controls exercise
supervisor cleanup separately. Native phase clocks retain explicit boot-clock
uncertainty. Native/child/Worker CPU, DO duration and full bill remain null.

## One tiny production proof

`/tmp/git-zero-fetch-stage3-tiny-production-20261018-01/proof.json` passes:
37,440 ms diagnostic elapsed time, successful case leader, complete drains,
no outer watchdog TERM/KILL, no remaining owned group and both ports free.
The three-generation fixture contains 32,790 working-tree bytes including
attributes. Its own SHA-1 wants/trees were sealed before Git construction and
all verified before production services started:

| Generation | Want | Tree |
| --- | --- | --- |
| 1 | 251d6a964118be89f5ef600728b38c73569ed755 | 99d8b9553583c66deef2eb4c870344a59a234233 |
| 2 | 2d286831e542700b67e830ea66d63fabab7c93a5 | 4b83bbf3852f4b92269c358dca09bde21c894d71 |
| 3 | 02fcada1fbc1ce4e931c765db850f16517321524 | 4130603aa8dd155fd172ea6ccd0820906c7ea3ce |

Its nine operations were three atomic pushes, three persistent latest reads,
and fresh no-have, seeded-history and seeded-latest reads. Every read installed
a nonempty retained pack and passed want/tree, strict fsck and full archive
hash checks. Git v0, postBuffer 268435456, `--keep`, unpackLimit 1 and frozen
local seeding semantics apply. This tiny plan does not claim historical large
wants or latency-tail reproduction. Its plan SHA-256 is
`cb98865a65c1d714fb56b9424a2ec81f19198d18fd13ef1a13fcde774f978547`.

Live PGID 993491 covered Native, Node wrapper/CLI, esbuild, both workerd
processes and Git descendants. Actual Native environment was eligible, with
only the four known Git settings plus `GIT_ZERO_PHASE_LEDGER`. Runtime receipts
retain actual Git 2.53.0, Node 24.10.0, Wrangler 4.149.0 and workerd identities.
The ledger has 127 records and 61 actual request-hash records, no ledger errors
or truncation; its clock uncertainty is 10,042,630 ns. Applicable final metrics
include one advertisement certificate, two advertisement shortcuts, 18 witness
hits, two consumed read preparations from six plans, and 176 lineage bytes.
Receive preparation consumed zero of three plans, with three unsupported;
this is reported as observed rather than requiring an imagined fast-path count.

Startup accounting reuses the first push's before-observer, and each operation
keeps the frozen before/after/post-verification observer order. No startup
probe or inventory was inserted into the prefix. Object inventory occurs only
after the target. Local source-archive hash comparison adds local Git work
outside the timed gateway command and no service traffic. The final unregister
and observer close all 12 total-versus-phase counter identities. Startup,
ordinary workload, and observer/cleanup stats remain explicit. These are local
emulator accounting receipts; they do not measure paid cloud CPU or a bill.
Native was ordinarily reaped after TERM and the Node wrapper after exit 143,
with complete streams and no surviving group.

## Remaining large runtime is unapproved

The executor copies the frozen Rust fixture functions and schedule/config
semantics and checks their source identities. A future case constructs all 32
generations and verifies every sealed want/tree before starting services. Any
mismatch stops. It then follows every original push/read/observer operation
through the target, retaining all prefix events. The two sealed plans remain
unchanged: cold64 generation16 has 42 operations (16 pushes/26 reads), target
`38d11e717df6687e26ade7033be62a56a408595c`; warm16 generation32 has 86
operations (32 pushes/54 reads), target
`6d55590087ff6851e7e37884c864b854d1c43f51`. Their actual reconstruction,
request/pack hashes, large runtime environment and latency are still null.
Current R19 cache/fast-path shape may differ from historical R18; no callback
count, R2 GET, old-provider identity or old-sample reproduction is forced.

The future batch interface requires separate runtime GO, exact source/ELF
hashes, both fixed sealed-plan hashes, one or two replicas and a fresh approved
eight-GiB raw prefix. A negative authorization check failed before creating its
proposed prefix. No batch execution occurred. Tiny-used source regression
controls passed: normal 113 ms, blocked socket 2385 ms, orphan-held pipe 2368 ms,
and injected controller-error cleanup; all owned descendants/listeners were
removed. The unchanged metadata-only final build was not rerun.

Resource checks preserve a two-GiB disk reserve, stream caps and fixture/output
capacity checks at construction/operation boundaries. Future large cases
require eight GiB available RAM and an aggregate eight-GiB owned raw-data cap.
That cap is sampled by the executor, not a filesystem quota. Historical source
packs were about 61 MiB cold and 37 MiB warm; they do not establish peak replay
usage. Forecast up to two GiB per live case conservatively, with four cases
serial and retained evidence counting toward the aggregate cap. No tmpfs prefix
has been authorized. Current disk fluctuation requires a fresh capacity check
before any future GO.

Tiny01 began with 6,484,701,184 disk bytes and 57,824,124,928 available memory
bytes; its last receipt reported 4,041,924,608 and 55,924,842,496 respectively.
Case logical output was 1,662,850 bytes under its 16-MiB cap. External disk
changes are not attributed to the case. Final metadata rebuild preflight had
2,617,548,800 free disk bytes and required the two-GiB reserve plus 256 MiB
temporary build headroom. R20 and primary worktrees were untouched.

The external `/tmp/git-zero-fetch-stage3-handoff-20261018` directory seals all
549 tiny files and 26 supervisor-control files without rewriting their
receipts, plus authorization, metadata continuity, static checks and final
archive/commit receipts. The accompanying JSON contains exact evidence hashes.
