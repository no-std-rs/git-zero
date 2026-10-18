# Development and validation

Run shell examples from the repository root unless shown otherwise.

The [Rust hosting experiment](../examples/repository-host/README.md) is covered
by `cargo test -p git-zero-host --locked`. It tests publication, rejected pushes,
compressed compaction, recovery, storage/ref limits, bounded caches and the
local/Cloudflare service contract. Lifecycle tests exercise repeated pushes,
restart, varied fetches and abandoned-write cleanup. See the
[benchmark commands](hosting-performance.md) for separate performance measurements.

## Validation

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check -p git-zero --target wasm32-unknown-unknown --locked
cd examples/cloudflare-worker
npm test
```

`cargo test` runs the eight-way native interoperability matrix plus sixteen
upstream-inspired differential cases in parallel, defaulting to the CPU count. All
HTTP tests need permission to bind/connect to loopback. The Worker tests clone
through the actual Wasm module with real Git over both HTTP protocols. See
[tests/README.md](../tests/README.md) for selective runs, CI sharding, and the optional
Git upstream test runner. CI also checks the core against a bare-metal target,
which has neither `std` nor an allocator.

## Local Cloudflare service contracts

The hosting crate intentionally ignores five real-service tests in its default
suite. Start [local Wrangler/workerd](../examples/repository-host/cloudflare/README.md)
with a persistent directory, then run the Rust contracts explicitly:

```sh
export GIT_ZERO_CLOUDFLARE_ORIGIN=http://127.0.0.1:8798
export GIT_ZERO_CLOUDFLARE_TOKEN=git-zero-local-services
cargo test -p git-zero-host --locked --test cloudflare real_ -- --ignored --test-threads=1
```

The other two tests require separate commands around a complete Wrangler process
restart. Choose a fresh namespace and keep it unchanged between phases:

```sh
export GIT_ZERO_CLOUDFLARE_RESTART_REPO=restart-proof-1
cargo test -p git-zero-host --locked --test cloudflare seed_before_workerd_restart -- --ignored
# Stop Wrangler, restart with the same --persist-to directory, and wait for /health.
cargo test -p git-zero-host --locked --test cloudflare verify_after_workerd_restart -- --ignored
```

Missing configuration fails these explicit runs.
The idle-eviction contract makes no calls for 13 seconds, then proves that the DO
instance changed while complete cumulative counters and their aggregate epoch
survived. Explicitly incomplete accounting is rejected by trace and cost helpers.

Accounting checks separate observer operations from workload usage. SQL row
counts and current SQLite bytes are measured; monthly SQLite storage retention,
Worker CPU and Durable Object active GB-seconds remain unmeasured. Cost reports
keep those monthly components unknown instead of presenting a complete bill.

Protocol references: [Git HTTP](https://git-scm.com/docs/http-protocol),
[Git pack format](https://git-scm.com/docs/pack-format),
[Cloudflare Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/).
