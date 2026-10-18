# Read/write repository hosting

The [hosting example](../examples/repository-host/README.md) accepts ordinary
SHA-1 Git pushes, clones and fetches. It uses Rust/native Git for pack processing,
with interchangeable local storage and Cloudflare service adapters. The portable
`no_std` library remains a separate read-only client/server implementation.

## Storage and publication

Keep compressed packs and indexes in immutable, content-addressed chunks rather
than storing every Git object separately. This preserves Git compression while
allowing unchanged chunks to survive later pushes. A generation-checked manifest
names the live chunks and refs; local files or Durable Object SQLite hold that
manifest, while local blob storage or R2 holds the bytes.

A push validates incoming objects in quarantine, stores immutable chunks, then
publishes the new manifest atomically. Retired chunks remain recorded as debt
until cleanup succeeds. Ambiguous publication or cleanup requires recovery from
authoritative state before further mutations. Reconstructed native repositories
are disposable caches. Local authoritative writes sync files and directories.

A push may compact four small packs within an 8 MiB input budget. Compaction
copies validated compressed entries without inflating every object; it does not
perform a full-history repack or remove unreachable objects inside live packs.
This trades some storage for less rewrite traffic and bounded maintenance work.

## Platform boundary

`BlobStore` and `StateStore` isolate storage and publication. `LocalServices`
provides the filesystem twin; `R2BlobStore` and `DurableStateStore` delegate through
`CloudflareBindings`. Git negotiation and storage policy stay outside those
bindings. The [local Cloudflare setup](hosting-cloudflare-services.md) runs real
workerd bindings with this layout:

```text
Git client -> Worker gateway -> Rust/native Git codec
                                   |
                    Worker service adapter -> Durable Object / R2
```

The codec runs outside workerd. A deployable Worker-only codec, production
authentication and concurrent reader leases remain future work. The current
host serves requests serially with one codec writer per repository.

## Reads, caches and limits

Fetches reuse compressed data and a bounded response cache. Cache identity
includes the manifest generation and request details; pushes invalidate it.
`warm` mode retains the reconstructed native view, while `cold` mode reloads
published chunks through the storage adapter. Historical reads should not
publish storage changes; the benchmark verifies this alongside fetched content.

Current native limits are 64 MiB per incoming request, 128 MiB of live compressed
packs plus indexes, and 16,384 refs / 4 MiB of ref data. Native chunk size is
16 MiB. Responses share a 32 MiB cache with at most 256 entries and a 16 MiB
per-response cap. Larger native packs require an additional materialized view.
Whole-pack buffers and native Git's working memory are outside a Worker memory
budget; bounded chunk size alone does not bound total memory.

Full Linux kernel history is not supported by these limits. Supporting it needs
streaming import and pack access, paged indexes, bounded recovery, and measurements
on the real repository before cloud deployment. A per-object R2 layout would
lose pack compression and multiply requests, so chunked compressed storage
remains the starting point.

Use [benchmark and cost guidance](hosting-performance.md) to evaluate the
storage/operation tradeoff. Local emulation counts binding operations; it does
not establish production latency, a complete cloud bill or a speed advantage
against tuned Git.
