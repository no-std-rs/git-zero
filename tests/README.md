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
This advances the testing roadmap; benchmarks and Wasm memory-growth tracking
remain separate work.

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
remain visible; Git's compressed loose storage and Rust's raw file representation
and encoder are still different implementations. No Python HTTP server is involved.

Each workload owns its ports, processes, clones and measurement files. Workers
share only the immutable synthetic fixture. `--case`, `--git`, `--binary`,
`--harness`, `--samples`, `--warmup` and `--jobs` permit separate runners.
`--response-limit` sets the response budget; request and object-read buffers
are each 64 KiB. Timing defaults to one workload worker
to avoid contention. Pair order alternates every round; warmups are discarded.
The JSON report retains every wall/CPU/RSS sample, median/p95 summaries, the
zero/Git latency ratio, tool/binary identity, source revision and host metadata.
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

The JSON schema is version 2 and preserves raw client/server samples, startup
measurements, host/tool/binary identities, buffer budgets, fixture parameters and
fingerprints. `--fixture-dir` retains Git's repository, raw export and metadata
in a new directory. To use both routes manually after creating that fixture:

```sh
target/release/examples/benchmark-server /tmp/git-zero-synthetic \
  "$(git --exec-path)/git-http-backend" both smart /tmp/git-zero-server-metrics.jsonl 33554432
# Append /git/repo.git or /zero/repo.git to the printed listening URL.
```

These are warm loopback measurements, with no cold-cache claim. Git-zero's raw
fetch is still not compared with Git's full materialization. The caller-provided
Linux kernel benchmark, advanced compression/delta modes, simultaneous process
tree memory and Wasm memory growth remain roadmap work.

## Wasm and allocator-free targets

```sh
rustup target add wasm32-unknown-unknown riscv32imac-unknown-none-elf
cargo check -p git-zero --target wasm32-unknown-unknown --locked
cargo check -p git-zero --target riscv32imac-unknown-none-elf --locked
cd examples/cloudflare-worker
npm ci
npm test
```

Worker tests compile a real Wasm module and use real Git against a Node HTTP host
of the exact Worker handler. This validates the shared ABI/protocol and concurrency;
Wrangler dry-run packaging checks the Cloudflare bundle. It is not a deployed-edge
integration test. The bare-metal build validates that the core needs no `std`;
Wasm compilation/linking succeeds without supplying any global allocator.

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
