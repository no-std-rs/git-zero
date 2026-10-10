# Testing and interoperability

Use Rust, Python 3, Node.js 22 or newer, and real Git with HTTP support. The native
HTTP tests start `git http-backend`, a static HTTP server, and git-zero processes.
No Apache, containers, credentials, or external service is required for the local
matrix. Network isolation must permit local loopback sockets.

```sh
cargo test --workspace --locked
```

Core tests run under Cargo's normal parallel runner. The integration test invokes
`interop.py`, which runs these independent cases with four workers:

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

## Selective runs and CI sharding

```sh
cargo build -p git-zero-rustix --bin git-zero --locked
python3 tests/interop.py --jobs 8
python3 tests/interop.py --jobs 2 --case git-zero-smart --case zero-git-smart
python3 tests/interop.py --case zero-git-dumb
```

Case names are `<client>-<server>-<protocol>`, with `git`/`zero` and `smart`/`dumb`.
`--binary /absolute/path/to/git-zero` chooses a release or separately built binary.
Independent invocations can run simultaneously; separate `--case` selections
also allow CI jobs to shard the matrix.

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
tests is a roadmap item, with feature mapping and license/provenance handling
for any future extracted GPL fixtures. The local matrix is the current git-zero
compatibility gate.
