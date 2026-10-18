# Documentation

Start with [usage](usage.md) to run the native demo, select crate features, or
try the existing Cloudflare metadata Worker. The library server currently
serves read-only repositories; the hosting documents describe the separate
read/write hosting work and its local verification.

| Topic | Guide |
| --- | --- |
| Current main capabilities and detailed examples | [Current capabilities](current-features.md) |
| Existing capabilities, workspace, and memory ownership | [Architecture](design.md) |
| Crate features, native/file-backed demos, metadata Worker | [Usage](usage.md) |
| Snapshots, refs, validation, selection, streaming, pack caches | [Embedding](embedding.md) |
| SHA-1/SHA-256, zlib, loose objects, packs, deltas, client parsing | [Object decoding](object-decoding.md) |
| Read/write hosting, service boundaries, pack storage, repacking | [Hosting design](hosting-design.md) |
| Local hosting comparisons and storage economics | [Hosting performance](hosting-performance.md) |
| Pushable local Worker gateway, R2/DO counters and cost estimates | [Cloudflare emulator](../examples/repository-host/cloudflare/README.md), [measurements](hosting-cloudflare-benchmarks.md) |
| Commit, measure and compare hosting costs | [Optimization loop](hosting-optimization-loop.md), [interleaved push/history-fetch protocol](hosting-interleaved-benchmarks.md) |
| Build checks and interoperability | [Validation](validation.md) |

Existing native benchmark methodology and decoder measurements live in the
[test guide](../tests/README.md#performance-comparison), with the shallow/filter
comparison in [its report](../tests/shallow-filter-benchmarks.md).

The [roadmap](../ROADMAP.md) tracks implementation status. Component details
remain with the [core](../crates/git-zero-core/README.md),
[client](../crates/git-zero-client/README.md),
[server](../crates/git-zero-server/README.md),
[native adapters](../crates/git-zero-rustix/README.md),
[metadata Worker](../examples/cloudflare-worker/README.md), and
[Linux kernel example](../examples/linux-kernel/README.md).
