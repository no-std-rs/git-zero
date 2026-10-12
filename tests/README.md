# Testing and interoperability

Use Rust, Python 3, Node.js 22 or newer, and real Git with HTTP support. The native
HTTP tests start `git http-backend`, a static HTTP server, and git-zero processes.
No Apache, containers, credentials, or external service is required for the local
matrix. Network isolation must permit local loopback sockets.

```sh
cargo test --workspace --locked
python3 -m unittest discover -s tests -p 'test_*.py'
```

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
also use that blob with 17-byte core read scratch and validate its checkout. The
current native HTTP host still needs a complete response buffer, independently
of the core object-read budget; streaming host I/O remains roadmap work.

Builder tests check arrival-order independence, unchanged historical objects,
slot capacity and failed-insertion atomicity, nested validation, exact commit
messages, distinct author/committer metadata, 64-bit times, and header injection
rejection. Real Git imports a custom builder snapshot and accepts `fsck --strict`.
The allocator-free Worker uses the builder with default metadata, so its existing
clone and object checks also exercise the builder in Wasm.

Repository validation tests cover SHA-1 mismatches, missing/wrong-kind links,
duplicate IDs, unreachable objects, malformed commit/tree records, canonical
modes, byte-based ordering, nonconsecutive file/directory name duplicates, binary
names, refs, exact scratch limits and backend failures. Deep trees and long parent
histories use iterative traversal with bounded scratch; a 1 MiB generated object
validates without a full-body buffer. Git's strict pack importer and `fsck --strict`
accept a validated streamed merge history with shared empty trees and an external
gitlink. Native nested/generated clone tests serve validated repositories; the
native demo and allocator-free Worker also validate before serving, preserving IDs
and pack/HTTP bytes across native and Wasm execution.

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

Core cache tests cover HEAD/encoder key separation, eviction, scratch and cache
capacity exhaustion, read/write backend failures, and output failure followed by
a scratchless retry. Hits do not enter the object provider. Failed generation or
insertion produces no pack output; a successfully inserted complete pack survives
an output failure. These synchronous tests leave concurrent host coordination,
corrupt cache entries and persistent backend policy for future work.

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

Use optimized native binaries and opt into the Linux benchmark (Python's
standard library plus GNU `time`). Python creates the fixture and runs clients;
all timed HTTP traffic passes through the Rust host. Correctness checks and
control barriers run outside timing.

```sh
cargo build -p git-zero-rustix --bin git-zero --release --locked
cargo build -p git-zero-rustix --example benchmark-server --release --locked
python3 tests/benchmark.py --samples 31 --warmup 3 \
  --fixture-dir /tmp/git-zero-synthetic --output /tmp/git-zero-performance.json
python3 tests/benchmark.py --jobs 8 --samples 5 --output /tmp/git-zero-contention.json
```

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

Git's loose/pack compression defaults to 0 and deltas are disabled to match
Rust's stored-DEFLATE encoder as closely as possible. `--git-compression 6`
measures a different compression tradeoff explicitly. Response body byte counts
remain visible. Fixture creation rewrites loose objects through Git's
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
`startup.frontend_peak_rss_kib`. Each limit checks the largest raw measured
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

These are warm loopback measurements, with no cold-cache claim. Git-zero's raw
fetch is still not compared with Git's full materialization. The caller-provided
Linux kernel benchmark, advanced compression/delta modes and simultaneous process
tree memory remain roadmap work. Wasm linear-memory budgets are checked separately
by the Worker tests described below.

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
and reproducible xorshift inputs against four shared targets.
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
the same bounded scratch. These targets cover existing APIs; zlib inflation and
pack-entry/delta parsing are still roadmap work.

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

This runs those upstream suites unchanged against Git itself. It is a baseline,
not additional git-zero coverage. Direct substitution of git-zero into those
tests remains a roadmap item. The [coverage map](upstream-http.md) groups their
assertions into supported comparisons, planned features and operations outside
the initial scope. Our independently authored fixture adapter uses real Git
commands and HTTP servers, without copying GPL test bodies. The parallel local
matrix and differential cases are the current git-zero compatibility gate.
