# Repository hosting experiment

A Rust local smart HTTP host for ordinary SHA-1 repositories. Pushes pass through
a quarantined native Git codec, then publish compressed pack/index chunks and an
atomic manifest. Reads use native Git and a bounded response cache. Four small
packs trigger compressed-entry compaction in Rust, without inflating their
objects. Replaced chunks are reclaimed after publication.

```sh
# From the workspace root; no npm, JavaScript runner, or cloud account required.
cargo build --profile hosting -p git-zero-host --bins --locked
target/hosting/repository-host /tmp/git-zero-host 8787
```

The server prints its bound URL. Use it with `git push`, `git clone` and
`git fetch`, for example:

```sh
git push http://127.0.0.1:8787/repo.git HEAD:refs/heads/main
git clone http://127.0.0.1:8787/repo.git /tmp/git-zero-clone
git -C /tmp/git-zero-clone fsck --strict
```

Run the local comparison with the same Rust HTTP runner for both backends:

```sh
target/hosting/hosting-bench --samples 5 --fixture-mib 4 \
  --pushes 32 --random-fetches 16 \
  --output /tmp/git-zero-host-benchmark.json
cargo test -p git-zero-host --locked
```

`BlobStore` and `StateStore` separate compressed storage from publication.
`LocalServices` syncs files and directories; `R2BlobStore` and
`DurableStateStore` delegate to a `CloudflareBindings` interface tested with local
mocks and [actual local Wrangler/workerd services](cloudflare/README.md).
That hybrid accepts pushes through a Worker gateway and exposes object inventory,
R2/DO operation counts and cost estimates; Git processing stays in Rust/native Git.
The portable Worker codec and async Rust SDK bindings remain work.
Use one process per storage root. The serial loopback runner buffers requests
(64 MiB cap), limits live packs/indexes to 128 MiB and refs to 16,384 / 4 MiB.
Single-chunk native files share
immutable blob storage. Native Git's expanded processing is outside a Worker memory
budget. This example supplies no authentication or remote listener.

Cold Cloudflare upload discovery can use manifest metadata after one complete
native protocol-v0 advertisement certifies the exact branch set, fixed main HEAD
and capability order. A bounded native type check also requires branch targets
to be commits. The learned native agent and capability template retain no refs
or objects. Strict native branch publications can advance the certificate;
recovery, external generations or changed codec/configuration invalidate it.
Tags, unborn repositories, other protocols and unrecognized requests or policies
keep the ordinary native path. Fetch POSTs still reconstruct the complete view.

`/metrics` reports advertisement shortcuts, native bootstrap attempts and the
one-time type-check count/time. `cache_bytes` and `cache_entries` describe the
existing response cache and clear after publication. The separate
`upload_advertisement_template_bytes` and `cache_retained_bytes` include the
bounded certificate in the same 32 MiB allocation budget. No certificate is
persisted, and eligible discovery writes no authoritative state or blobs.

See the [hosting design](../../docs/hosting-design.md) for the Cloudflare service
layout and Linux-size cost assumptions, and the
[performance report](../../docs/hosting-performance.md) for results and limits.
The [Cloudflare emulator measurements](../../docs/hosting-cloudflare-benchmarks.md)
record actual binding operations, object statistics and cost scenarios.
