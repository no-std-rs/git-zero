# Stage-two hard supervisor and replay plans

Stage two adds only a separate Linux diagnostic supervisor binary and this
handoff. Approved Native/hooks, six common Worker modules, stage-one runner,
benchmark/generator/scorer, dependencies and lockfile remain byte-identical to
`a827df032e87374737b1ac7e36fb518b507e8411`. No production fix, expensive
fixture, replay, latency result, cost claim or billing measurement is included.

## Immutable history before builds

Before any edit/build, stage-one actual Native/runner ELFs and all 680 tracked
files were copied independently into:

`/tmp/git-zero-fetch-diagnostics-immutable/stage1-a827df032e87374737b1ac7e36fb518b507e8411-v1`

Read-only `relocation-map.json` has 682 entries and SHA-256
`420c4a5aa3327daf19dbdf02eb77d168767e8acabf6aa02d75f63d366c5e8840`.
All copies freshly verify. Original proof/review/manifests remain unchanged.
Original paths are historical locations, not timeless hash claims after future
builds. The current stage-one ELF paths still match their approved hashes.
The final stage-two source/ELF also have a versioned read-only archive and
relocation map recorded in the accompanying JSON.

## What the watchdog proves

`fetch-phase-supervisor` is a dedicated process outside a newly owned case
process group. Case work belongs inside that killable group. The parent polls
process status and pipe-drainer completion; it never performs case socket reads
or joins a pending drainer. A work deadline triggers TERM followed by KILL
(after 250 ms) within a separate fixed cleanup grace. Group members must have
fresh PID/start-time ownership and current parent-chain evidence. PGID <=1,
the parent's group, and changed/lost generations are rejected. No signal is
issued after ownership is lost. ESRCH is distinguished from other signal
errors. EINTR returns to bounded polling; work is not retried.

A cleanup guard covers early controller errors after spawn, uses the same
absolute work-plus-cleanup end, preserves partial stdout/stderr, and joins
only finished drainers. The ordinary timeout path records the direct leader's
`Child::try_wait` status and reaps adopted descendants only by exact verified
PID. The early-return guard can reap the direct child after the last possible
`Child::try_wait`; it never resumes work afterward. There is no `waitpid(-1)`.

Root explicitly authorized a minimal Linux-only unsafe FFI module for
`PR_SET_CHILD_SUBREAPER`, `kill` and nonblocking `waitpid`. It has explicit
safety comments, uses the C ABI/long prctl arguments, and adds no dependency.
It changes no Native/core/library code. References:
[child subreapers](https://man7.org/linux/man-pages/man2/PR_SET_CHILD_SUBREAPER.2const.html),
[waitpid](https://man7.org/linux/man-pages/man2/waitpid.2.html),
[kill](https://man7.org/linux/man-pages/man2/kill.2.html).

This is an external watchdog over stalled case code, not a hard real-time OS
guarantee. Scheduling delay and a blocked/uninterruptible controller-side
`/proc` or filesystem syscall can delay polling/receipt output. Linux /proc
visibility and children inheriting the owned PGID are prerequisites. No setsid
or PGID escape exists in the cheap case path; an expensive case executor must
preserve that property. If safe ownership or cleanup cannot be proven, evidence
is incomplete and work must stop. Missing/failed observers stay null rather
than zero/free. Native/child/Worker CPU, DO duration and full bill remain null.
Native phase clocks retain stage one's explicit boot-clock uncertainty; these
supervisor elapsed durations do not provide a new cross-process clock.

## Final bounded cheap evidence

`/tmp/git-zero-fetch-stage2-supervisor-proof-20261018-02/proof.json` passes.
Each normal/stall control uses a two-second work deadline and separate
two-second cleanup grace. Actual results:

| Control | Elapsed | Result |
| --- | ---: | --- |
| Normal child | 78 ms | exit 0, complete drains, no signal |
| Blocked socket read | 2416 ms | TERM ignored; KILL, descendant adopted/reaped |
| Orphan holds stdout/listener | 2685 ms | leader exited 0; blocked drain still timed out, orphan KILL/reaped |
| Injected controller error | bounded early error | direct child and descendant KILL/reaped; partial logs retained |

The resistant children are actual processes, with `/proc` receipts proving
TERM ignored and matching PGID. The fixed shell wrapper is confined to making
these cheap test children ignore TERM; signaling/reaping uses the supervisor
FFI, without external kill helpers. Two controls produce over 256 KiB on
stdout while only 128 KiB prefixes are retained; drains still complete.
The injected error cleanup also completes both joins and retains partial
capture with incomplete/null counter receipts. All groups disappear, leaders
and adopted descendants are reaped, and 8853/8854 bind checks pass after each
control. No Native/Worker/Git production runtime was started in stage two.
Proof 01 is retained as an earlier three-control pass; final source uses 02.

## Exact plans, with reconstruction still unverified

`/tmp/git-zero-fetch-stage2-case-plans-20261018-02/plans.json` seals two input
JSON files. Report hashes, full fixture generation metadata, immutable schedule,
all prefix operations, target want/tree, original configuration and historical
prefix timings are retained. The frozen Rust hosting-bench and schedule source
files are byte-identical to historical R18 source. Plan construction recomputes
the schedule from the sealed 32 tips and compares it to the original report;
it does **not** generate Git objects or prove regenerated commit identities.
`fixture_reconstruction_verified` therefore remains null.

| Case | Original sample | Prefix through target | Sealed target |
| --- | --- | --- | --- |
| cold64 generation 16 no-haves | R18 sample 1 | 42 ops: 16 pushes, 26 reads | history-9, want 38d11e717df6687e26ade7033be62a56a408595c, tree 141c23fa72d6958422cd6699001ddd410cf703a5 |
| warm16 generation 32 no-haves | R18 sample 1 | 86 ops: 32 pushes, 54 reads | history-4, want 6d55590087ff6851e7e37884c864b854d1c43f51, tree 66c8fe43b4875bc980a381d59b8dac5aefcf900a |

These are plans for current diagnostic R19 plus approved hooks. Provider source
differs from historical R18: no old provider/sample identity is reused, and no
exact old-provider reproduction is claimed. Before any case, reconstruct the
full 32-generation source using frozen content/commit/config/GC semantics,
verify all sealed wants/trees, then replay the complete preceding push/read and
observer order into fresh independent services/Native state. A mismatch must
stop execution, not relabel the fixture. Keep every slow prefix event.

Plans require original Git v0, 268435456-byte postBuffer, packed nonempty fetch
retention (`--keep`, unpackLimit 1), persistent latest clients, fresh historical
clients and original local ancestor seeding, strict fsck/want/tree/archive
checks, actual production gateway and applicable lineage/certificate/prepare
receipts. Actual R19 cache shape, fast paths and read bytes must be reported;
no forced R2 GET or latency/callback count is prescribed. Original packet traces
are absent, so historical request hashes remain null; later actual request and
pack hashes must be captured. Trace2/curl-no-data applies only to child client
commands. Unknown inherited Git variables must be removed, and every future
Native cohort must retain its actual environment while alive. No Native cohort
was launched here; future Native environment receipts are null.

Proposed limits are at most two replicas per case (four total), 1200 seconds of
work per case including fixture setup, prefix, Git, socket/drain waits and
observers, plus five seconds fixed cleanup grace. Unexpected timeout stops the
batch without retry or continuation. The serial upper work-plus-grace budget is
4820 seconds. This binary currently exposes only cheap proof and plan sealing;
there is no expensive replay CLI or reconstructed-fixture claim.

Resource forecast: the sealed cold fixture has a 64 MiB working tree and~61 MiB
source pack; warm has 16 MiB and~37 MiB. Those source sizes are historical context,
not measured replay peak usage. Propose an 8 GiB cap for a separately approved
owned raw-data prefix, at least 2 GiB disk reserve, and at least 8 GiB available RAM
before a future serial case; allow conservatively up to 2 GiB per live case for
source, Native view, emulator state, clients and evidence, with cap checks by
its future executor. Current disk fluctuations make an 8 GiB disk allocation
unsafe without a fresh check. No tmpfs prefix is authorized or used. R20's
prefix/state/processes were untouched. All actual stage-two build/proof work
stayed above the 2 GiB reserve; the new proof/plan files total under 1 MiB.

Stop here for independent source/proof review and separate root GO. A future
fixture/execution implementation must itself be frozen and reviewed before
expensive runtime is enabled.
