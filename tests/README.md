# Testing and interoperability

Use Rust, Python 3, Node.js 22 or newer, and real Git with HTTP support. The native
HTTP tests start `git http-backend`, a static HTTP server, and git-zero processes.
No Apache, containers, credentials, or external service is required for the local
matrix. Network isolation must permit local loopback sockets.

```sh
cargo test --workspace --locked
python3 -m unittest discover -s tests -p 'test_*.py'
RUSTFLAGS=-Dwarnings python3 scripts/check-features.py
python3 scripts/check-public-packages.py
```

The feature matrix checks 39 separate builds, including empty features and
client-only/server-only consumers, so workspace feature unification cannot hide
a missing feature gate. Tests declare their required features and run when
those capabilities are selected. Release tests check version alignment across
all public crates, Cargo upload framing, sparse-index feature preservation and
immutable release asset retries.

Core tests run under Cargo's normal parallel runner. The integration test invokes
`interop.py`, which runs these independent cases with one worker per available CPU:

| Client | Server | Smart HTTP | Dumb HTTP |
| --- | --- | --- | --- |
| Git | Git `http-backend` / static bare repo | Clone, fetch, fsck | Clone, fetch, fsck |
| Git | git-zero `rustix` | Clone, fetch, fsck, single root commit | Clone, fetch, fsck, single root commit |
| git-zero | Git `http-backend` / static bare repo | Discover, fetch, Git index-pack/fsck | Discover, fetch loose HEAD, zlib/hash check |
| git-zero | git-zero `rustix` | Discover, fetch, Git index-pack/fsck | Discover, fetch loose HEAD, zlib/hash check |

The client currently stops at downloaded pack/loose data. Its dumb cases do not
claim to clone the entire repository. A tiny proxy forces dumb discovery in the
git-zero client/server case. Real Git uses `GIT_SMART_HTTP=0` to select that path.

The native `head_states` suite runs eight independent server comparisons: unborn
HEAD on the default/custom branch and detached HEAD with/without a branch ref,
each over smart and dumb HTTP. `server_head_states.py` creates the Git oracle
using Git's own object and ref commands. Real Git compares all discovery refs
and symrefs, clone HEAD state, object IDs/kinds/bodies, checkout contents, strict
fsck and detached HEAD fetches. Empty clones contain no refs or objects. The suite
runs throughout the Linux/macOS and system/pinned Git CI matrix. Failures retain
fixtures, packet/curl traces and HTTP/command logs in the reported temporary path.
Core tests enforce unborn name/packet budgets, reject all empty-repository wants
before output, validate empty/detached graphs and preserve state through indexes
without allocation. `http-backend` emits an empty raw `/HEAD` for unborn refs;
git-zero serves the symbolic HEAD file, matching Git's static dumb repository.

The native `references` suite compares symbolic, detached and unborn HEAD with
independent branches, merge histories, symbolic chains (including HEAD through
a lightweight commit tag), annotated/nested tags,
and tree/blob targets over smart and dumb HTTP. `server_references.py` generates
the oracle using Git's commands and exports immutable raw bodies to the native
file provider. Discovery, raw ref files, clone/fetch refs, checkouts and every
object's ID/kind/body are compared, with strict fsck. Single-branch clones and
matched raw upload requests verify selected object closures; duplicate/multiple
wants share objects. Git's reference smart client can request `include-tag` even
with `--no-tags`, so its clone may receive extra tags peeling to the selected
closure; matched requests without that capability require exact physical objects
on both servers. This suite runs across the same Git/platform CI matrix and
retains fixtures, packets, curl traces, HTTP logs and downloaded packs on failure.
Core `references` tests cover wrong/missing peels, symbolic cycles, malformed
tags, advertised-want policy, capacities, byte-read exhaustion, retries, independent
request storage and rejection of unsafe HEAD-key cache/whole-pack reuse. Allocation
tracking covers ref construction, graph validation, selection and HTTP serving.

The [server acceptance matrix](server-acceptance.md) records coverage and remaining
gaps for each server milestone, independently of git-zero client limitations.
`server_negotiation.py::compare_negotiation` compares v0 single/multi/detailed
ACK rounds, both sideband modes, common-history exclusion and incremental fetch
against Git's HTTP backend in both object formats. It runs inside the native
`references` suite and retains raw responses and imported packs on failure.
`tests/upload.rs` adds allocation, phase/truncation, capability conflict, packet
size, checksum, exclusion-reset, graph budget and ERR-packet regressions.
Worker memory tests repeat negotiated ACK/sideband requests in both formats;
the real-Git Worker smart clones cover v0/v1/v2.

`server_protocol.py` extends the native references suite with v1/v2 discovery,
ls-refs prefix/symref/peel/unborn queries, full and incremental clones/fetches,
matched ACK rounds and exact pack imports. Both formats cover empty/unborn,
symbolic/detached HEAD, independent roots and nested tags. Git can gzip requests
over 1 KiB; native tests use its normal compression behavior. Host request storage
bounds compressed plus expanded bytes. `protocol` checks malformed commands,
selectors/media types, unsafe paths, graph/output budgets and retries without
allocation. Gzip tests use Python's independent encoder and validate truncation,
CRC/size/header fields and expansion limits. Worker v1/v2 ABI tests enforce the
same fixed memory ceiling in both formats, alongside real-Git SHA-1 clones.

`server_shallow.py` adds independent Git comparisons for v0/v1/v2 shallow clones
at depths 1, 2 and beyond the history, including merge tips, unchanged shallow
fetches, absolute and relative depth increases and unshallowing in SHA-1/SHA-256. It compares
shallow files, delivered commit/tree/blob IDs and bodies, checkouts and strict
integrity. Git v0 may additionally send include-tag annotations. Partial clones
use --no-checkout and test blob:none, blob:limit=5, scaled limits and combined
shallow/filter requests; lazy retrieval of unadvertised promised blobs remains
unsupported. Core `references::bounded_fetch` covers shorter merge paths in
both enumeration orders, explicit blob wants, nested tags, budgets, malformed
options, retries, allocation counts, exact boundary control capacities, repeated-buffer state
isolation and guarded one-byte pulls. Worker real-Git
clones test depth/unshallow and partial fetches; ABI tests repeat them in both
formats across fragmented pulls, cancellation and independent instances within
the unchanged 24-page memory ceiling.
Relative selection follows Git 2.55: only wanted history determines the nearest
client boundary. The multi-boundary/single-refspec case explicitly asserts the
older Git 2.53 oracle's extra branch deepening, git-zero's preserved unrelated
boundary, and identical reachable object bodies/checkouts. Matched all-branch
deepening requires identical shallow files on every Git version.


`tests/inflate.rs` compares the core zlib decoder with Python's standard zlib
and a real Git-created loose object. Stored, fixed, dynamic and mixed blocks run
with one-byte input/output, overlapping copies, a 32768-byte back-reference,
history wraps, 15-bit codes, single-code/empty-distance alphabets and code-length
repeats crossing table boundaries. It checks truncations, checksums, malformed
trees, reserved codes, dictionary rejection, exact/zero output caps, saturating
ratio/slack budgets, chunk-independent failure positions, trailing bytes and
reset isolation. A thread-local allocator counter covers construction, streaming
and reset with fixed buffers; a size assertion caps decoder state at 2 KiB,
independently of its caller-owned 32 KiB history window. Existing Adler-32 encoder
regressions remain in `tests/zlib.rs`.

Loose-object tests check all four kinds, canonical 64-bit headers, exact buffers,
ID/body/header mismatches, checksum failures, trailing streams, transport status/
length errors and allocation-free fetch/decode. The real Git loose-object fixture
also passes through the validated client decoder.

Pack-reader tests generate both offset-delta and reference-delta packs with real
Git, require actual deltas, strictly import them, then compare every computed
entry CRC and canonical ID with Git's v2 index and every reconstructed body with
`cat-file`. Commit/tree/blob/tag entries are all covered. Synthetic cases cover
two delta levels, same-pack base identity, external reference bases, multi-byte
offsets, sparse copy fields, default 65536-byte copies, missing/wrong bases,
malformed sizes/varints/instructions, exact/overrun capacities, output/CPU/depth
budgets and cursor-preserving retries. Allocation tracking covers pack reading,
hashing, CRC computation and delta resolution with fixed caller storage.

Each case owns its temporary directory, Git configuration environment, bound
ephemeral port, child processes, and timeouts. Ports are reported only after the
socket is bound, avoiding port reservation races. Servers and directories are
cleaned up on success or failure. The Git fixture has two commits, another branch,
a tag, and a large compressible file; separate pack tests use empty, duplicate,
and multi-block binary data. Git's own `index-pack --strict` checks generated packs.

The native snapshot integration test also clones nested trees with executable
files, symlinks, repeated subtrees, empty trees, and a custom default branch plus
branch/tag aliases through both HTTP protocols. It checks modes, symlink targets,
ref discovery, fetches, and `fsck --strict`. Git's dumb walker can consider the
empty tree available through its built-in object cache without saving it to disk
(observed with Git 2.53; see upstream [odb.c](https://github.com/git/git/blob/v2.53.0/odb.c)
and [walker.c](https://github.com/git/git/blob/v2.53.0/walker.c)). The dumb test
explicitly downloads that loose object through the native git-zero client and
saves the server's bytes before the disk integrity check; smart clone needs no
supplement. Core tests cover tree ordering, duplicate names, exact buffer limits,
depth limits, ref validation, and unchanged object IDs for equivalent flat inputs.
The native demo and nested clone test use caller-owned metadata indexes. Index
tests check capacity and missing-object errors, preservation of enumeration order
and HTTP/pack bytes, and repeated reads without reentering the source repository.
The Python tests cover the LM-CC/token report handoff, including full precision,
incomplete/invalid measurements, checksum validation, and bounded comment output.
They also reproduce a PR file shadowing Python's `json` module and verify that
the isolated producer ignores it while preserving the measured ratio.

Reader tests cover interleaved cursors, resumable hashing, short reads, backend
retries, invalid read counts/early EOF, empty scratch, and 64-bit offsets. Generated
objects match borrowed snapshot IDs and pack/HTTP bytes with read budgets from
one byte to 64 KiB, across stored-DEFLATE block boundaries. A 1 MiB generated blob
streams directly to Git's pack importer or loose files using bounded scratch;
both object stores pass `fsck --strict`. Native smart/dumb clone and fetch tests
also stream that blob over HTTP with 17-byte output staging and validate its
checkout. The reference-ref suite uses 37-byte staging and 8 KiB of control storage
instead of a complete response buffer in both formats and all protocol versions.

`pack_streaming.rs` compares pull and synchronous pack/loose encoders across stored
block boundaries, short reads, one-byte output slices, selection orders and both
formats. `streaming.rs` checks guards, allocation counts, state-size bounds, backend retry,
independent cursors and terminal cancellation. Request intake checks fragment
splits, length/EOF bounds and cancellation. The HTTP fuzz target replays fragmented
intake and guarded response pulls. Native tests cancel disconnected streams.
Worker ABI tests cover start/read/cancel, invalid capacities, restart, legacy
compatibility and isolated instances within the existing 24-page ceiling.

Builder tests check arrival-order independence, unchanged historical objects,
slot capacity and failed-insertion atomicity, nested validation, exact commit
messages, distinct author/committer metadata, 64-bit times, and header injection
rejection. Real Git imports a custom builder snapshot and accepts `fsck --strict`.
The allocator-free Worker uses the builder with default metadata, so its existing
clone and object checks also exercise the builder in Wasm.

Repository validation tests cover hash mismatches, missing/wrong-kind links,
duplicate IDs, unreachable objects, malformed commit/tree records, canonical
modes, byte-based ordering, nonconsecutive file/directory name duplicates, binary
names, refs, exact scratch limits and backend failures. Deep trees and long parent
histories use iterative traversal with bounded scratch; a 1 MiB generated object
validates without a full-body buffer. Git's strict pack importer and `fsck --strict`
accept a validated streamed merge history with shared empty trees and an external
gitlink. Native nested/generated clone tests serve validated repositories; the
native demo and allocator-free Worker also validate before serving, preserving IDs
and pack/HTTP bytes across native and Wasm execution.

`sha256` tests independently hash every nested snapshot object with Git in both
formats, import packs with `index-pack --strict`, and run `fsck --strict`.
They cover mixed-format refs/peeled IDs/hash metadata, binary tree and commit/tag
link widths, full 32-byte reference-delta bases, SHA-256 trailers, and exact
format-specific pkt-line limits. Builder, indexes, short-read validation, hashing,
pack decoding and cache miss/hit paths run under allocation tracking. The graph,
loose and pack-entry fuzz assertions also exercise SHA-256 with dedicated seeds.
Native `references` runs all HEAD/ref/tag/selected-closure comparisons in SHA-1
and SHA-256, plus SHA-256 empty remotes with ordinary and custom unborn branches.
The independently generated fixtures avoid redundant tips to stay below Git's
1 KiB request-gzip threshold for the native host, which currently takes decoded,
Content-Length-delimited request bodies. The Git reference wrapper decompresses
gzip requests before invoking `http-backend`.

Native storage tests persist raw object bodies and compare their IDs and generated
pack/loose bytes against a 1 MiB generated repository. They exercise interleaved
and concurrent positional readers, ranges in shared files, sparse offsets above
4 GiB, invalid ranges, missing/nonregular files, final symlinks, replacement/unlink,
and truncation. Prepacked tests reject corrupt headers, bodies and trailers,
truncated/extra bytes and the wrong object closure, and prove reuse reads only the
verified pack. A thread-local allocation counter checks file opening, validation,
prepacked verification and pack generation/serving with fixed caller buffers.
Real Git clones/fetches nested file-backed snapshots through smart and dumb HTTP,
with and without a prepacked file, and passes `fsck --strict` in all four cases.

The changing-snapshot test keeps the same native HTTP URL while selecting
immutable file-backed snapshots between completed fetch sessions. Real Git
clones through smart and dumb HTTP, fetches A → B → A, checks the remote tracking
ref and single root history, verifies every object body and checkout byte, and
passes `fsck --strict`. The snapshots change a binary file, add/remove files and
share an unchanged blob. A fixed 4 KiB, single-entry pack cache serves repeat
downloads and regenerates each evicted HEAD exactly once; generated and cached
wire bytes match. A thread-local allocation counter covers all served routes.
This test runs across the existing Linux/macOS and system/pinned Git CI matrix,
using `GIT_ZERO_TEST_GIT` to select the reference executable.

Legacy core cache tests cover HEAD/encoder key separation, eviction, scratch and cache
capacity exhaustion, read/write backend failures, and output failure followed by
a scratchless retry. Hits do not enter the object provider. Failed generation or
insertion produces no pack output; a successfully inserted complete pack survives
an output failure. `selected_cache.rs` adds independent Python hashlib key
vectors, equivalent sets across provider orders, changed independent roots,
selected and empty packs, fixed-buffer disabled/eviction/capacity behavior and
bounded one-byte streaming reads. Allocation guards cover miss, hit and empty
paths; write-only feature tests ensure integrity needs no pack reader. Git's
strict importer independently checks both formats and every imported ID/body.
Truncations and malformed headers/zlib/Adler32/trailers never reach the sink;
valid recomputed checksums around wrong or duplicate objects still fail exact
set verification. Backend/sink failures, byte/ID budgets, failed-miss preservation
and scratchless hits retain retry behavior. Concurrent host coordination and
persistent backend policy remain future work.

The native `disk_cache` suite exercises persistent hits across independent
backend instances, disabled mode, eviction, bounded regular-file reads, corrupt
and truncated data, symlink rejection and capacity failures. Exclusive temporary
name collisions preserve foreign files; failed publication cleans its own
staging file and preserves the prior entry. Concurrent cold producers publish
complete packs with different valid object orders while readers verify exact
canonical sets. Resident readers remain independent through eviction/replacement;
sink failure retains a complete persistent pack for retry. Allocation guards
cover these backend operations, and independent Git imports and object-body
comparisons run in both formats with storage-only Rust features.

Native coordination tests hold independent producer leases for the same and
different keys, recheck a published entry to avoid duplicate generation, and
release after cancellation or failed generation without allocation. Disabled
mode creates no lock file; symlink, directory and nonempty lock entries fail
without changing foreign bytes. A deterministic duplicated-descriptor test
checks explicit unlock while another reference to the open file remains.

## Selective runs and CI sharding

The eight original matrix cases run alongside sixteen independent differential
cases. See [upstream-http.md](upstream-http.md) for the pinned upstream coverage
map, exact comparisons, expected unsupported operations and parallelism contract.
The new cases compare all refs and object bodies with Git, exercise merge history,
proven delta packs and packed dumb baselines, change the remote between fetches,
and compare native snapshot output with independently Git-generated objects.

```sh
cargo build -p git-zero-rustix --bin git-zero --locked
python3 tests/interop.py --jobs 8
python3 tests/interop.py --jobs 2 --case git-zero-smart --case zero-git-smart
python3 tests/interop.py --case zero-git-dumb
python3 tests/interop.py --case upstream-packed-smart
python3 tests/interop.py --list-cases
python3 tests/interop.py --shard 0/2 --artifacts /tmp/git-zero-traces
python3 tests/interop.py --shard 1/2 --artifacts /tmp/git-zero-traces
python3 tests/interop.py --git /opt/git-2.53/bin/git
```

Case names are `<client>-<server>-<protocol>`, with `git`/`zero` and `smart`/`dumb`.
`--binary /absolute/path/to/git-zero` chooses a release or separately built binary.
Independent invocations can run simultaneously; separate `--case` selections
and zero-based `--shard INDEX/TOTAL` selections allow disjoint CI jobs. Artifact
directories stay unique even across simultaneous invocations of the same case.
`--artifacts` preserves fixture repositories, downloaded bytes, command logs,
HTTP requests and packet/curl traces. The CI matrix spans Linux/macOS, installed
Git and pinned Git 2.53.0, and two independent shards. Cargo accepts the same
configuration through `GIT_ZERO_TEST_JOBS`, `GIT_ZERO_TEST_GIT` and
`GIT_ZERO_TEST_ARTIFACTS`.

## Performance comparison

The [shallow/filter regression comparison](shallow-filter-benchmarks.md) records
before/after results for both existing profiles and a repeated smart-clone CPU check.

Use optimized native binaries and opt into the Linux benchmark (Python's
standard library plus GNU `time`). Python creates the fixture and runs clients;
timed HTTP traffic passes through the Rust host. Correctness checks and
control barriers run outside timing.

```sh
cargo build -p git-zero-rustix --bin git-zero --release --locked
cargo build -p git-zero-rustix --example benchmark-server --example decoder-read --release --locked
python3 tests/benchmark.py --samples 31 --warmup 3 \
  --fixture-dir /tmp/git-zero-synthetic --output /tmp/git-zero-performance.json
python3 tests/benchmark.py --jobs 8 --samples 5 --output /tmp/git-zero-contention.json
python3 tests/benchmark.py --git-profile upstream-defaults --samples 31 --warmup 3 \
  --output /tmp/git-zero-upstream-defaults.json
```

Timing runs require Git **2.53.0**, pinned in `tests/git-version.txt`; select its
binary with `--git`. A version mismatch fails before fixture preparation.
`--allow-unpinned-git` explicitly permits system-Git correctness smoke tests;
the report records the expected/actual version and whether the pin was required.
CI runs both profiles with pinned Git and retains their reports. System-Git
smoke coverage remains separate from those pinned comparisons.

The default synthetic repository has 64 files of 16 KiB, 16 commits, mixed
repetitive/binary content, and one changed file per later commit. `--files`,
`--blob-bytes` and `--commits` control scale. Git creates and strictly checks the
repository; reachable bodies are exported into `objects.raw` and `objects.tsv`
for the existing Rust file provider. Both backends have identical object IDs,
history, refs and checkout contents. Rust validates the exported graph before
listening, reads bodies by file offset, and generates responses without a
prepacked cache. Fixture creation/export is outside measurements; separate server
startup wall/CPU/RSS observations include Rust's graph validation.

The same Rust executable parses requests, buffers responses and writes HTTP
headers/body for both backends. Smart Git requests invoke `git-http-backend`;
dumb Git requests serve its loose files directly through Rust, without CGI.
The native backend calls core HTTP/pack functions using `FileRepository` and
`ValidatedRepository`. All clients request protocol v0. The host can route
`/git/repo.git` and `/zero/repo.git` together, or enable one backend per process.
The benchmark uses separate instances of that same host so unused backend state
and previous requests cannot contaminate the other backend's memory counters.

Eight independently runnable workloads compare both protocols. Client discovery
holds the Git-backed Rust server fixed and compares Git/git-zero ref output.
Server discovery, fresh clones and unchanged-remote fetches hold the real Git
client fixed. Every measured operation checks refs or the complete reachable
object graph, strict integrity, HEAD and checkout bytes/modes outside timing.

Two additional local workloads, `client-object-read-loose` and
`client-object-read-pack`, compare Git's `cat-file --batch` with `decoder-read`.
Both read the **same encoded files** and emit every expected ID, kind and body;
the runner compares complete batch output outside timing. These exercise zlib,
loose-object envelope/ID validation, pack checksums/entry CRC computation and
offset/reference delta resolution. Pack preparation uses a separate owned bare
repository, leaving the HTTP loose fixture intact. Its fingerprint, byte count,
delta count and maximum chain depth are recorded. Both profiles use identical
raw bodies; the matched pack contains no deltas, and CI requires actual deltas
in the upstream-default pack.

Local-read timings include fresh process startup, file I/O, host allocation,
decoding, materialization and batch output. Rust additionally authenticates
canonical IDs/checksums and uses bounded host slots with linear base lookup;
Git uses its existing object database/index. They measure complete object reads,
not isolated inflater CPU or a complete HTTP client fetch. Client wall/CPU/RSS
are reported without invented server/startup counters. The example bounds object
count to 8192, raw bodies to 64 MiB and pack base/instruction slots to 128 MiB,
and rejects fixtures exceeding that host staging budget.

The default `--git-profile matched` selects compression 0 and disables deltas
on both servers. `--git-profile upstream-defaults` leaves Git's compression,
window and depth settings unset; its HTTP comparison measures the current
stored-block server against Git's default compressor/delta search. The report
explicitly marks whether server compression/delta features are equivalent.
Local-read inputs remain identical between readers in either profile.
`--git-compression`, `--git-delta-window` and `--git-delta-depth` permit additional
Git configurations, labeled `custom`; they cannot accompany upstream-defaults.
Algorithm descriptors and exact config overrides stay in each report so future
server encoders can join the same comparison. The current server encoder is
stored DEFLATE without deltas. Response body byte counts remain visible.
Fixture creation rewrites loose objects through Git's
`hash-object` to apply compression consistently, checks stored-DEFLATE headers
at level 0, and records their size/fingerprint as well as the raw export's.
Git's compressed loose storage and Rust's raw file representation
and encoder are still different implementations. No Python HTTP server is involved.

Encoded file responses use Linux `sendfile` by default (`--file-transfer auto`).
Short transfers and interrupted calls resume at the current offset. Unsupported
syscalls/filesystems fall back to bounded positional reads and socket writes;
`--file-transfer buffered` forces that path. The dedicated host ignores SIGPIPE
so disconnected clients produce an error, and it rejects truncated file bodies.
Per-request sendfile/fallback byte counts show the path actually used.

Rust normally generates loose objects on request. Add `--zero-loose-cache` to
encode them once during each Rust server's startup and serve those immutable
files using the same sendfile/fallback path as Git. This explicitly compares
file serving on both sides; cache creation CPU/RSS remain in startup metrics.
This option only affects dumb HTTP. Smart Rust packs remain generated on request.

Add `--zero-selection` to use bounded requested-closure serving in the Rust
host. The default keeps the complete-repository pack path for comparisons with
older revisions. Both modes use the same workloads and correctness checks.
Reports identify the pack policy and charge the selected mode's want, metadata,
index and parse buffers to startup; its graph-read limit is 64 MiB per request.
The 64 KiB object-read buffer is shared with ordinary serving. Selection storage
is allocated once by this `std` benchmark host; the core still borrows buffers.

Each workload owns its ports, processes, clones and measurement files. Workers
share only the immutable synthetic fixture. `--case`, `--git`, `--binary`,
`--harness`, `--samples`, `--warmup` and `--jobs` permit separate runners.
`--response-limit` sets the response budget; request and object-read buffers
are each 64 KiB. Timing defaults to one workload worker
to avoid contention. Pair order alternates every round; warmups are discarded.
The JSON report retains every wall/CPU/RSS sample, median/p95 summaries, the
zero/Git latency ratio, tool/binary identity, source revision and host metadata.
CPU affinity and system load averages are recorded, including the load before
each sample, so shared-host contention stays visible when interpreting latency.
Client CPU/RSS remain separate: GNU time excludes inherited Python launcher
memory, while client CPU includes the small time observer. The Rust host records
per-request wall time, `getrusage(RUSAGE_SELF)` and `RUSAGE_CHILDREN` CPU deltas,
including reaped CGI/upload-pack descendants, response bytes and page faults.
Read/dispatch/write are measured; resource reporting and barriers run afterwards.
The host owns CGI stdin/stdout and kills/reaps unfinished children on errors;
the runner stops each server process group on completion or failure.

Server memory reports current frontend RSS and its process-lifetime high-water
mark from `/proc`, plus the largest reaped child's lifetime `ru_maxrss` in KiB.
These high-water marks include preparation/warmups and are not per-operation
increments, simultaneous aggregate RSS, or unique physical memory. **Do not add
frontend and child peaks.** No CPU/RSS claim is inferred from client counters.
Git's smart CGI process interface remains part of the measured backend cost;
the shared frontend does not turn Git into an in-process server library.

`--rss-sample-ms 10` opts into Linux server process-tree sampling (10–1000 ms;
the default 0 disables it). Each HTTP operation samples its own live frontend,
all thread child lists and recursive descendants. `server.process_tree_rss`
records the largest RSS sum from **one sweep**, process counts, sample count,
actual maximum gap, and observer wall/CPU overhead. It never adds independent
lifetime peaks. Sampling starts after the metric barrier and stops before
reporting; startup, control requests, the client and the Python observer are
outside this server scope. The observer runs in the runner, so its CPU is not
charged to server or client CPU. Its synchronous initial/final reads are outside
client timing; background reads can perturb execution, which is why this is
opt-in and the overhead is retained.

These are sampled lower bounds, not atomic snapshots or exact memory ceilings.
Short-lived CGI processes and between-sweep peaks can be missed, and shared
pages count in every process's RSS; no unique physical/PSS or JS-isolate memory
claim follows. Smart Git operations that never observe a live CGI descendant
report `status: partial`, retain `observed_peak_rss_kib`, and leave
`peak_rss_kib` null. Partial observations cannot pass a sampled-tree budget or
produce a complete peak summary. Each sweep is bounded to 256 processes, 1024
tasks, 1 MiB of reads (64 KiB per file), and 250 ms; the observer sleeps between
sweeps. Read/permission errors, a replaced/exited frontend or exceeded bounds
fail the workload and retain an explicit report error rather than silently
publishing a smaller sum. The kernel's
[CONFIG_PROC_CHILDREN interface](https://github.com/torvalds/linux/blob/master/fs/proc/base.c)
is required: a missing children file for a still-live task fails explicitly;
a vanished task remains a tolerated exit race. Cancellation/timeouts stop the observer and clean up
the owned command/server process groups, including descendants left after the
frontend exits.

Sequential local observer controls, without overlapping local tests or scoring,
used pinned Git 2.53.0, unchanged release host/client
binaries from `e3dc303`, the matched profile, 16 files × 65,536 bytes × 8
commits, 10 paired samples, two warmups and one workload worker. Every run had
the same 39 objects, 1,514,520 raw bytes and fixture fingerprint
`153972d7110b0bd10d61f6881b2046c027cfcce0d3b291b103cfe462af5e8abb`.
The `server-clone-smart` median wall observations were:

| Runner | Git ms | git-zero ms |
| --- | ---: | ---: |
| Unchanged base, first control | 118.81 | 86.62 |
| New runner, observer disabled | 65.12 | 52.59 |
| New runner, 10 ms observer | 50.83 | 40.68 |
| Unchanged base, repeated control | 43.75 | 35.38 |

The unchanged base controls also moved substantially on this shared host;
these observations do not isolate a latency regression or speedup. Both HTTP
backends and release binaries were identical. The enabled observer recorded a
median 1.53/0.96 ms of its own CPU per Git/git-zero operation, with individual
sweeps below 1.11 ms. Its largest observed tree sums were 16,392/3,776 KiB with
four/one simultaneous processes. Two Git operations missed live descendants
and were explicitly partial; their aggregate budget values remain null. These
remain sampled RSS observations rather than exact physical-memory peaks.
Reproduce the enabled run with:

```sh
python3 tests/benchmark.py --files 16 --blob-bytes 65536 --commits 8 \
  --case server-clone-smart --samples 10 --warmup 2 --jobs 1 \
  --rss-sample-ms 10 --output /tmp/git-zero-tree-observer.json
```

The JSON schema is version 3 and preserves raw client/server samples, startup
measurements, host/tool/binary identities, buffer budgets, fixture parameters and
fingerprints. `--fixture-dir` retains Git's repository, raw export and metadata
in a new directory. A failed workload leaves completed cases and explicit errors
in a report marked `failed`, and the command exits unsuccessfully.

`--limits path.json` adds explicit regression checks to a run. The file uses
`schema_version: 1` and a nonempty `limits` array. Each entry selects one executed
`workload`, `implementation` (`git` or `zero`), `metric` and inclusive numeric
`maximum`. For example, `tests/benchmark-limits.json` caps the tiny CI fixture's
Rust smart-clone client and frontend at 65,536 KiB each, and requires no backend
child memory. Run it with the CI fixture dimensions:

```sh
python3 tests/benchmark.py --files 4 --blob-bytes 256 --commits 3 \
  --case server-clone-smart --samples 2 --warmup 1 \
  --limits tests/benchmark-limits.json --output /tmp/git-zero-budget-check.json
```

Supported metrics are `client.wall_s`, `client.cpu_s`, `client.max_rss_kib`,
`server.wall_s`, `server.cpu_s`, `server.frontend_peak_rss_kib`,
`server.backend_child_peak_rss_kib`, `startup.wall_s`, `startup.cpu_s` and
`startup.frontend_peak_rss_kib`. `server.process_tree_peak_rss_kib` additionally
checks valid sampled aggregate observations and requires `--rss-sample-ms`.
For example, `--rss-sample-ms 10 --limits tests/benchmark-tree-limits.json`
adds a 65,536 KiB sampled-tree cap to the tiny CI fixture's Rust smart clone.
Each limit checks the largest raw measured
observation, so a single outlier fails even when the median passes. Startup
limits check the one startup observation for that server. Wall/CPU observations
exclude discarded warmups; lifetime RSS peaks still include preparation/warmups.
Client, frontend and child memory stay separate. In client-discovery workloads,
both implementations use the same Git-backed server, as in the paired samples.

Limits are opt-in observations, checked after the workloads finish. Choose
wall/CPU caps for your fixture and host; CI enforces memory caps without imposing
timing caps on shared runners. Unknown metrics, duplicates and limits for
unselected workloads fail before servers start. Missing, invalid or exceeded
observations fail the run. `resource_limits` records the policy and
`budget_evaluations` records every observed maximum, sample count and outcome;
all raw samples remain in the failed report. Unit tests cover boundary values,
outliers and incomplete results; CI exercises passing caps and an impossible
zero-KiB cap through the real Rust host and Git clients.

To use both routes manually after creating that fixture:

```sh
target/release/examples/benchmark-server /tmp/git-zero-synthetic \
  "$(git --exec-path)/git-http-backend" both smart /tmp/git-zero-server-metrics.jsonl 33554432 auto generated
# Append /git/repo.git or /zero/repo.git to the printed listening URL.
```

### Pinned caller repositories

Opt into an existing local SHA-1 repository instead of the synthetic fixture.
Supply the full lowercase commit ID; branch names and abbreviated IDs are
rejected, so moving HEAD or another ref cannot select a different workload.

```sh
python3 tests/benchmark.py --source-repo /path/to/linux \
  --revision 'FULL_40_CHARACTER_COMMIT_ID' \
  --max-source-objects 8192 --max-source-bytes 67108864 \
  --case server-clone-smart --case server-fetch-smart \
  --samples 5 --warmup 1 --output /tmp/git-zero-pinned-performance.json
```

The importer reads the selected commit's complete reachable graph, including
merge parents, from Git's object store. It ignores working-copy changes and
replacement refs, disables lazy fetching and transport access, and rejects
shallow repositories, legacy graft files, borrowed alternate object stores
(including shared clones), or missing objects. It creates a separate bare repository
with that commit at HEAD/main and the same raw export used by the native host.
Source refs, config, index, packs and worktree remain unchanged. Reports and
retained fixtures must be outside the source and its Git directories, including
shared worktree metadata; symlinked destinations are resolved before checking.
Extra source
branches and annotated tag objects are excluded from the selected commit graph.
Checkout verification covers nested paths, arbitrary path bytes, executable
files and symlinks; external gitlinks remain uninitialized directories.

The defaults cap enumeration at 8192 objects and retained raw bodies at 64 MiB.
Enumeration stops at the object bound; object sizes are checked before loading
all bodies. Increase both limits explicitly for a larger selection, and size
the independent HTTP response budget with `--response-limit`. The fixture report
records the source path, exact commit, complete history count, limits and the
existing raw/encoded fingerprints. These input limits do not describe Python's
total preparation memory; the current native host also owns one descriptor per
object. Fixture preparation remains outside request timing.

Git reads source packs and deltas during preparation. Both prepared backends
retain identical IDs and bodies, and the benchmark's compression/delta policy
still applies to the separate Git fixture. This measures the prepared backends,
rather than the caller repository's original storage layout. Synthetic
`--files`, `--blob-bytes` and `--commits` cannot be combined with this mode.

Ten real-Git unit tests check packed merge history, source preservation,
moving refs/replacements, exact and exceeded limits, invalid selections,
overlapping destinations, shallow/SHA-256 repositories and unavailable promised
objects. They also reject shared/transitive alternates and grafts in common
worktree metadata before enumeration or writes. CI runs all
eight paired HTTP workloads plus both local-read workloads against a small
packed caller fixture, checking
its nested binary files, symlink, executable and external gitlink. These remain
warm-server/cache measurements; kernel-scale preparation and controlled
cold-cache runs remain roadmap work.

These are warm loopback measurements, with no cold-cache claim. Git-zero's raw
fetch is still not compared with Git's full materialization. The caller-provided
Linux kernel benchmark's remaining preparation work, additional server encoders
and simultaneous process tree memory remain roadmap work. Wasm linear-memory
budgets are checked separately by the Worker tests described below.

### Decoder and Git-default comparison

Revision `ebe634d` was measured with pinned Git 2.53.0, release builds
(`opt-level = "s"`, LTO, one codegen unit), 21 alternating pairs, three warmups
and one workload worker. Both policies use the same 111-object, 1,341,030-byte
raw fixture and independently check every object's ID, kind and body.
The upstream-default pack contains 19 deltas with maximum depth 15.

| Workload, median wall ms | Matched Git | Matched Rust | Default Git | Rust against default Git |
| --- | ---: | ---: | ---: | ---: |
| Smart HTTP clone | 104.18 | 62.48 | 182.13 | 88.63 |
| Dumb HTTP clone | 101.92 | 105.63 | 138.48 | 129.49 |
| Local loose-object reads | 16.70 | 38.81 | 19.89 | 35.75 |
| Local pack-object reads | 9.25 | 41.09 | 15.15 | 42.19 |

Server clone latency remains competitive on warm loopback, while the new local
readers are slower than Git. Default-Git smart/dumb clone responses total
648,543/674,949 bytes versus Rust's 1,343,340/1,344,644 bytes: the stored-block
server sends about twice as much data. This bandwidth difference matters away
from loopback. Local-read CPU medians against default Git are 8.27/17.50 ms
(Git/Rust loose) and 5.92/21.89 ms (Git/Rust pack); Rust's extra authentication
and host materialization are included. These observations do not isolate which
decoder phase dominates.

A separate alternating baseline/current comparison used identical server
fixtures, response byte counts, compiler (rustc 1.97.1), release profile and 21
clone pairs. It compared pre-decoder `242eabd` with `ebe634d`:

| Rust clone | Baseline wall ms | Current wall ms | Baseline server CPU ms | Current server CPU ms |
| --- | ---: | ---: | ---: | ---: |
| Smart HTTP | 24.93 | 25.48 | 1.288 | 1.513 |
| Dumb HTTP | 40.34 | 39.91 | 1.987 | 2.147 |

Wall medians differ by about +2%/-1%; server CPU rose about 17%/8%
(0.225/0.160 ms). This is a measured CPU increase even though encoder source
behavior is unchanged. The shared host was loaded, so these runs establish
neither a universal latency ranking nor an isolated cause for the CPU increase.
Raw reports and retained encoded fixtures are under
`/tmp/git-zero-pr16-benchmark/{matched,upstream,revisions}.json` on the measuring
host. Reproduce the paired profiles with the commands above; the reports retain
all samples, binary fingerprints, pin/config policies, host load and buffer limits.

### Decoder read optimizations

Revision `6355c25` batches stored-block and back-reference emission, history
copies and Adler-32 updates. Small Huffman prefix tables use already available
bits without speculative input reads; resumable state remains below 2 KiB.
Pack CRCs process eight bytes per step with an 8 KiB constant table. Output and
expansion budgets still stop before the first excessive byte, with identical
failure positions across chunk partitions. Independent zlib/Git oracles,
all-`0xff` checksum boundaries, guarded output, allocation tracking and a
bitwise CRC reference cover the optimized paths.

The host reader buffers 64 KiB of batch output, allocates exact declared entry
sizes through `Reader::next_entry_size`, and allocates reconstruction slots only
for deltas. Header declarations are checked against the host budget before
allocation and validated against the inflated stream. Offset bases use binary
search and reference bases use an ID index. The same fixture now uses
1,341,030/1,888,901 bytes of pack slots for matched/default inputs, down from
7,288,704 bytes. These host allocations remain included in CPU and RSS.

Both pinned 21-pair, three-warmup, one-worker ten-workload profiles passed after
the changes, including all object IDs, kinds and bodies and the 19-delta default
pack. Local-read medians, including the GNU time observer, are:

| Policy / local reads | Git wall ms | Rust wall ms | Git CPU ms | Rust CPU ms |
| --- | ---: | ---: | ---: | ---: |
| Matched loose | 30.18 | 26.49 | 8.21 | 6.03 |
| Matched pack | 22.55 | 24.94 | 5.65 | 7.76 |
| Default loose | 38.97 | 25.57 | 8.71 | 7.64 |
| Default pack | 30.00 | 25.67 | 6.14 | 7.74 |

Loose reads use less CPU than Git in both policies. Pack reads still use about
26–37% more CPU, including complete pack authentication, canonical object IDs
and host body retention. The heavily loaded host makes these wall medians
unsuitable for a general latency ranking. Full-profile raw reports and encoded
fixtures are under `/tmp/git-zero-pr16-perf-fix/{matched,upstream}.json` and
`{matched,upstream}-fixture` on the measuring host. They retain decoder binary
fingerprints; both runs record an uncommitted working tree based on `ac8697e`.
Their decoder fingerprint matches the binary built from `6355c25`.

A separate 21-sample comparison rotates Git, the saved pre-optimization decoder
(`ac8697e`) and `6355c25` through identical retained loose/pack files, after three
warmups. It checks complete batch output outside timing and uses `wait4`
directly, excluding the GNU time observer. CPU medians are:

| Policy / local reads | Git CPU ms | Before CPU ms | After CPU ms | Reduction |
| --- | ---: | ---: | ---: | ---: |
| Matched loose | 7.44 | 20.10 | 4.84 | 76% |
| Matched pack | 4.85 | 26.43 | 6.86 | 74% |
| Default loose | 7.87 | 18.27 | 6.58 | 64% |
| Default pack | 4.94 | 20.92 | 6.41 | 69% |

All three processes have one median voluntary context switch. Default pack
minor faults fall from 1846 to 716 (Git: 207); user CPU falls from 13.63 to
2.31 ms and system CPU from 7.09 to 3.97 ms. Involuntary switches fall with
shorter runs, but remain sensitive to host scheduling. This points to reduced
decoder and allocation work as the measured improvement, with no evidence of
excess voluntary scheduling in the original reader. It does not identify
individual syscalls or prove that fault reduction explains all system CPU.
The comparison script, samples and binary fingerprints are retained in
`/tmp/git-zero-pr16-perf-fix/resources-{paired.py,paired.json,metadata.json}`.

Reports now retain separate user/system CPU, voluntary/involuntary context
switches and minor/major page faults in each sample and summary. `wait4` counts
include the command's observer and waited descendants, exclude the HTTP server,
and do not count syscalls. Both local readers have three median voluntary
switches with this observer. No major faults occurred in these warm runs.
[Git 2.53.0's packfile reader](https://github.com/git/git/blob/v2.53.0/packfile.c)
maps pack/index windows, while this host owns the compressed pack and retains
decoded entries. Mapped/indexed storage remains a possible I/O improvement;
these measurements do not isolate its benefit or measure cold-cache behavior.

### Isolating encoder CPU

The Linux `encoder-profile` example loads and checks the same raw export before
timing. It reports five rounds of process CPU and wall time per complete object
graph, with reusable output storage and a configurable repetition count. Copying,
the former scalar Adler-32 calculation, stored zlib, loose encoding, pack encoding
and bulk blob SHA-1 run separately. File reads, HTTP, fixture validation and output
logging are outside the timed loops. Compile with this repository's release
profile (`opt-level = "s"`, LTO and one codegen unit) when comparing revisions.

```sh
cargo build -p git-zero-rustix --example encoder-profile --release --locked
target/release/examples/encoder-profile /tmp/git-zero-synthetic 200 \
  > /tmp/git-zero-encoder-profile.jsonl
```

On Git 2.53.0 and the default 111-object, 1,341,030-byte fixture, the same profiler
built against baseline `f78a54e` and the batched encoder gave these median CPU
times (five rounds of 200 graphs, in milliseconds):

| In-memory operation | Baseline | Batched Adler-32 |
| --- | ---: | ---: |
| Copy bodies into reusable storage | 0.027 | 0.019 |
| Scalar Adler-32 reference | 2.900 | 3.029 |
| Stored zlib | 3.042 | 1.062 |
| Loose objects | 3.064 | 1.022 |
| Pack including SHA-1 trailer | 3.610 | 1.105 |
| SHA-1 of the complete export as one blob | 0.525 | 0.529 |

Two modulo reductions per input byte dominated the original encoder. The current
encoder reduces after at most 5552 bytes, the largest batch whose worst-case
second sum fits `u32` when both starting sums are reduced. This is also the
bound used by [zlib's Adler-32 implementation](https://github.com/madler/zlib/blob/v1.3.1/adler32.c).
It preserves stored-block boundaries, checksums and wire bytes, with the same
fixed memory. Tests cover all-`0xff` overflow boundaries, multiple blocks,
arbitrary part boundaries, short reader buffers and real Git imports.

Separate untraced HTTP runs used 31 alternating pairs, three warmups and one
workload worker. Each clone's graph and checkout passed Git validation outside
timing. Server CPU includes the frontend and reaped Git descendants:

| Clone workload | Git before | Rust before | Git after | Rust after |
| --- | ---: | ---: | ---: | ---: |
| Smart HTTP, CPU ms | 17.825 | 4.234 | 15.352 | 1.755 |
| Dumb HTTP, generated Rust loose objects, CPU ms | 3.355 | 5.798 | 3.134 | 3.151 |

These runs use identical fixture fingerprints and compression level 0 without
deltas. The host was heavily loaded, and Git's control measurements also moved;
use these as workload-specific CPU observations rather than general latency
rankings. After the change, median frontend lifetime peak RSS was 3960/3856 KiB
for Git/Rust smart clone and 2456/2492 KiB for dumb clone. Git's largest reaped
smart child peak was separately 5680 KiB; these peaks must not be added.

The original smart advantage has several measured causes. Git Trace2 records
two `upload-pack` commands and one `pack-objects` per clone; the host starts two
additional CGI processes, while the Rust route starts none. Rust validates its
immutable graph during startup. Git enumerates and prepares a pack per request,
and its [non-reuse packing path](https://github.com/git/git/blob/v2.53.0/builtin/pack-objects.c)
reads object bodies and compresses them again. Packing a separate copy of the
same Git fixture reduced its median smart CPU to 13.176 ms, versus 4.296 ms for
unchanged Rust. Storage layout therefore explains part of the difference.
Trace2 region durations are wall time, separate from untraced CPU measurements.

Hash implementations also differ: the installed Git binary includes
[SHA1DC collision detection](https://github.com/git/git/blob/v2.53.0/sha1dc_git.c),
while this machine's Rust `sha1` backend uses Intel SHA instructions without that
collision detector. A separate `git hash-object --stdin` experiment measured
1.595 ms CPU for an empty input and 5.054 ms for the export; the difference includes
file I/O and is not a pure checksum measurement. The in-memory blob hash above
produces the same object ID. These backend, preparation and storage differences
remain even though the HTTP frontend, logical repository and client are shared.

## Wasm and allocator-free targets

```sh
rustup target add wasm32-unknown-unknown riscv32imac-unknown-none-elf
cargo check -p git-zero --target wasm32-unknown-unknown --locked
cargo check -p git-zero --target riscv32imac-unknown-none-elf --locked
cd examples/cloudflare-worker
npm ci
npm test
```

Worker ABI tests cap initial linear memory at 24 Wasm pages (1.5 MiB), assert
64 KiB input/128 KiB output capacities, and require the memory buffer to remain
identical after calls. They repeat discovery, pack generation and loose-object
reads 128 times on one instance, check pack/object SHA-1 values, exercise an exact
64 KiB input frame and a large pack, and reject truncated/invalid frames and
over-capacity lengths before checking recovery. Two independent instances also
prove output-buffer isolation. These checks measure Rust Wasm linear memory;
JavaScript allocation and total isolate memory are separate host costs.

Worker tests compile a real Wasm module and use real Git against a Node HTTP host
of the exact Worker handler. This validates the shared ABI/protocol and concurrency;
Wrangler dry-run packaging checks the Cloudflare bundle. It is not a deployed-edge
integration test. The bare-metal build validates that the core needs no `std`;
Wasm compilation/linking succeeds without supplying any global allocator.

## Parser fuzzing

`cargo test -p git-zero --test fuzz --locked` embeds and replays the 32 independently
authored binary seeds, every prefix truncation, six byte substitutions at every position,
and reproducible xorshift inputs against seven shared targets.
`cargo test -p git-zero --test core --locked` also checks non-hex packet prefixes
at every position and the maximum packet-length boundary.
The same target functions back a separate cargo-fuzz workspace; core dependencies
remain allocation-free. See [fuzz/README.md](../fuzz/README.md) for formats, exact
buffer budgets, smoke campaigns and crash replay. CI runs 5,000 AddressSanitizer
iterations per target on a pinned nightly toolchain and retains crash artifacts.

Packet checks enforce hexadecimal framing, progress and non-consuming errors.
Client response checks exercise discovery, default-branch lookup, pack framing
and overreported transport lengths. Routing varies method, path, query, body and
output capacity. Graph mutations recompute object IDs so invalid commit/tree
records reach parsing, and compare borrowed versus short-read providers using
the same bounded scratch. Inflation checks resumable state and output/ratio
limits; loose/pack targets validate envelopes, entries and bounded delta
instructions. Pack-index file parsing remains roadmap work.

## Git's existing HTTP tests

Git maintains [t5550-http-fetch-dumb.sh](https://github.com/git/git/blob/master/t/t5550-http-fetch-dumb.sh),
[t5551-http-fetch-smart.sh](https://github.com/git/git/blob/master/t/t5551-http-fetch-smart.sh),
and [lib-httpd.sh](https://github.com/git/git/blob/master/t/lib-httpd.sh). Their
`update-server-info` and `http-backend` fixture approaches are reused here through
actual Git commands. Their shell assertions depend on a built Git source tree,
Git test helpers, Apache/modules, and features beyond this initial read-only core.
No upstream test source is copied into this project.

For a reference baseline, supply a **built Git source checkout** and installed
Apache dependencies to the optional runner:

```sh
sh scripts/test-upstream-git.sh /path/to/git-source 4
```

This runs those upstream suites unchanged against Git itself as a reference
baseline. A separate opt-in adapter runs six unchanged Git **v2.53.0** `t5551`
cases (2–7, including askpass-helper setup) against native git-zero and Git CGI
backends, over v0/v2 × SHA-1/SHA-256:

```sh
cargo build -p git-zero-rustix --example upstream-cgi --locked
python3 tests/upstream_harness.py \
  --source /path/to/built/git-2.53.0 --git /path/to/install/bin/git \
  --native target/debug/examples/upstream-cgi \
  --apache /usr/sbin/apache2 --modules /usr/lib/apache2/modules \
  --output /tmp/git-zero-upstream-fixtures
```

Run as a non-root Linux user with `/proc` child lists, Python pidfd support,
Apache 2.4 and its CGI/prefork modules (Ubuntu:
`apache2`). The native CGI and native reference comparisons use the same bounded
raw-fixture loader: 256 objects, 32 refs, 8 MiB of raw objects and 64 KiB per metadata
file, including empty, unborn, symbolic and detached HEAD states. The source checkout must be clean at the exact tag commit, with
`GIT-BUILD-OPTIONS` and `t/helper/test-tool` built. The output directory must be
new; it retains every trial's TAP, bare/client repositories, Apache logs, private
CGI trace and the final `report.json`, including failures. Each upstream shell
run has a 120-second deadline, configurable with `--timeout`; individual Git
inspection commands have 30-second deadlines. Detached daemon/CGI cleanup is
bounded and tested on normal completion and timeout. The dedicated Python runner
requires one thread, Linux child-subreaper adoption, `/proc` child lists and
`waitid(P_PIDFD)` support; it checks these before launching Apache. Each pidfd
must name a kernel-waitable child before it can be signalled. Killing an owned
parent adopts its remaining descendants for subsequent bounded cleanup passes.
Native failures, skipped
selected tests, absent/foreign traces or mismatched object graphs fail the run.
Apache's unchanged harness cannot inherit the selected port's reservation socket.
Only an exact chosen-port `EADDRINUSE` before any selected test or native trace
allows a new isolated trial, at most three attempts; failed logs and retry records
remain in the report. Semantic, native and timeout failures are never retried.
CI retains these directories in a compressed tar archive, preserving helper
symlinks and hidden client `.git` metadata instead of copying installed Git
executables through every helper link. Both Git's source and installation remain
unchanged. The [coverage map](upstream-http.md)
documents provenance, limits and the exact substitution hook. The broader suites,
HTTP/2, Worker and Apache's static dumb Alias remain separate coverage gaps.
