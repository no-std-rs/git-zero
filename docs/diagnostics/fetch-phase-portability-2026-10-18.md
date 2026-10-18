# Fetch phase runner portability and environment capture gate

Parent SOURCE e41159b7fcd3fc9ef16b189464fd6c54189af13b. Runner-only changes;
Native a827/R19+hooks, stage1 runner, six Worker modules/tree7f41, dependencies,
locks, fixture/schedule/observer/client order, protocol/cache/resource/timeouts
are unchanged. Original batch02 cold environment remains NULL and every original
proof, review, blocked launch and immutable archive remains intact.

Native environment capture now occurs after the existing readiness log, without
a service probe. Before/after live PID/start/PGID and actual /proc/PID/exe hashes
must match. It retains raw environ byte count/SHA256 and only exact safe canonical
Git values plus the owned ledger path. Empty/missing/duplicate/unknown/wrong Git
settings fail qualification; wrong and unknown values are withheld. Raw environ
is not saved. Failure receipts remain incomplete rather than substituting counters
for actual environment evidence.

A fixed embedded71-file SHA256 closure seals the complete Native modules/hooks,
core dependency source, Cargo manifests/lock, stage1 runner, frozen generator/
schedule, Worker six modules/config/package/lock. Its SHA256 is
4e8283e57be8c2b4e01c35b427de54c23c73ec62e344340aa2b970ce35330632.
The same two original sealed plan files are copied byte-for-byte into the runner
source directory, retaining their original fixed hashes. Execution still validates
all32 wants/trees before services and stops on mismatch.

Fresh-host builds use an explicit <=128KiB runtime manifest, not the previous
host's Native ELF hash or Wrangler /tmp path. It binds every actual runner source
file and the real Native/supervisor/Git/Node/Wrangler/workerd/installation-lock
paths/hashes; PATH tools and approved lock must match, and full source/runtime
receipts must remain equal before/after each case. Seal with
`--seal-runtime-manifest NEW_FILE`, explicit FETCH_PHASE_BUILD_SOURCE_COMMIT and
FETCH_PHASE_BUILD_PROFILE; optionally FETCH_PHASE_WRANGLER selects an installed
locked CLI. Supply FETCH_PHASE_RUNTIME_MANIFEST to tiny/batch. Future batch
config additionally binds runtime_manifest_sha256. Build commands/toolchain/
source provenance are separate operator receipts; manifest hashes are actual
identities and do not claim reproducible ELF derivation. CARGO_MANIFEST_DIR gives
the fresh checkout source location. Legacy historical plan-sealing CLI still
references its old local archive, and is not needed by portable execution.

One tiny proof passed at /tmp/git-zero-fetch-portability-tiny-20261018-01:
45,552ms diagnostic elapsed, same3 generations/32,790 bytes/9 operations and exact
prior tiny plan, all6 strict-index packs plus wanted/tree/strict-fsck/archive
checks. All104 commands/208 complete stream hashes,15 resource boundaries,
127 phase records/61 request hash records and full12-counter scope closure pass.
Applicable v0 evidence:1 certificate,2 shortcuts,18 witnesses,2/6 read consumptions,
2 projections/176 lineage bytes; receive0/3, unsupported. No imagined fast-path
counts are required. CPU/Worker duration/full bill remain NULL. Native clock
alignment remains approximately10ms uncertainty; elapsed stages are not CPU.

Actual Native3ccf43bb... was not rebuilt. New supervisor ELF543e1c53... and exact
source are independently preserved before runtime at
/tmp/git-zero-fetch-diagnostics-immutable/portability-tested-v1,695 aliases, map
9c44f153c9842041a44f96da7d91c681724041e24b2c5ddc300e3d7be7a92821.
Post-readiness actual environment was5,438 bytes, hash2d58c689..., all canonical4
plus ledger qualified, matching PID3108522/start57763925/PGID3108055 and actual
Native ELF before/after. Capture began108,604ns after the readiness receipt;
its1.939s interval includes two full ELF hash reads and is not CPU/service time.
The outer leader exited0 with complete drains/ordinary cleanup, no signals,
timeout, owned group or8853/54 listener.119 exact recorded PID/start pairs checked.

Four focused tests pass (empty/missing environment, secret value withholding/
wrong/duplicate/unknown settings, manifest framing/source-role pins and128KiB
cap/malformed JSON). Strict Clippy initially reports pre-existing frozen
socket_rpc::call too_many_arguments and supervisor type_complexity; allowing
only those two lints passes. Rustfmt and diff checks pass. Old stage2 stall
controls are reused: exact supervisor/canonicalization/control functions and all
19 checked executor fixture/IO/resource/operation helpers remain byte-identical.
No Rust edit or rebuild followed this tiny gate. The manifest records actual
parent HEAD at seal plus exact then-uncommitted runner hashes; the final clean
commit has these identical tested Rust bytes.

A disabled draft workflow and preparation notes live only at
/tmp/git-zero-fetch-standard-vm-draft-20261018-01. It has not been installed,
pushed or run. It prepares pinned Git2.53.0/Node22.20.0/Rust1.97.1, npm-ci locked
Wrangler, fresh release --locked Native/supervisor builds -j2 outside timing,
actual VM/tool/ELF/disk identities and compact <=4MiB artifacts. No raw R2/SQLite/
large Git/archive/pack/ELF export. It proposes conservative fresh launch floors
16GiB MemAvailable/10GiB real disk; an ordinary VM may BLOCK honestly. These
proposed launch values require a separate root resource/activation decision;
source8GiB memory floor,2GiB reserve,8GiB sampled raw cap,1200s+5s each and the
existing two serial once-each plans remain unchanged. Sampled guards are not
hard filesystem/RSS quotas; Linux scheduling and uninterruptible I/O limit the
supervisor's practical timing guarantee.

Local Node24/debug tiny proof does not validate the future Node22/release/VM disk
cohort, and no old-host debug ELF continuity is assumed. No new large fixture,
large replay, latency/speed/cost/acceptance claim, CI/push/deploy/main integration
or mandatory final5 was performed. STOP for fresh independent source/tiny review
and a separate root GO before any VM runtime or large case.
