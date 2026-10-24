# Development and validation

Run shell examples from the repository root unless shown otherwise.

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

With Node 22.13 or newer, run `node --test tests/runnerless.test.mjs` from the
repository root to check required policy recovery. GPU complexity comparisons
are retired; public CI uses GitHub-hosted runners.
The hosting suite runs separately with `npm ci` and `npm run test:streamed` in
`examples/repository-host/cloudflare`; it exercises local Wrangler R2/SQLite and
direct workerd fault-injection fixtures with ordinary Git clients.

Protocol references: [Git HTTP](https://git-scm.com/docs/http-protocol),
[Git pack format](https://git-scm.com/docs/pack-format),
[Cloudflare Wasm](https://developers.cloudflare.com/workers/runtime-apis/webassembly/).
