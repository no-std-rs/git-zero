# Local Cloudflare repository hosting

Git smart HTTP through local workerd, R2 and SQLite Durable Objects, backed by
the Rust/native Git codec. The same service interfaces support local storage.
Test and benchmark runners are Rust; no Cloudflare account is required.

Build from the workspace root:

```sh
npm ci --prefix examples/repository-host/cloudflare
cargo build --profile hosting -p git-zero-host --bins --locked
```

Follow the [local setup](../../../docs/hosting-cloudflare-services.md#run) to
start the emulator and codec, then push or clone through
`http://127.0.0.1:8798/demo/repo.git`.

- [Setup and restarts](../../../docs/hosting-cloudflare-services.md)
- [Benchmark commands and cost model](../../../docs/hosting-performance.md)
- [Hosting design](../../../docs/hosting-design.md)

The codec still runs outside workerd. Local measurements count binding operations;
production latency and the full compute bill remain unmeasured.
