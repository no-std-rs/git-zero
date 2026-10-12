# Bounded parser fuzz targets

This separate workspace uses [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html)
and libFuzzer with AddressSanitizer. It adds no dependency or allocation to the
core library. Target support lives in `tests/fuzz_support/mod.rs` and is shared
with `cargo test -p git-zero --test fuzz --locked`, which replays the checked-in
seeds, truncations, byte substitutions and deterministic binary inputs.

All seeds and assertions were authored for git-zero. No upstream Git test source
or GPL fixture has been copied. Seeds include valid smart/dumb discovery,
checksummed empty packs, upload requests, malformed prefixes, tree modes,
duplicate entries and truncated object IDs.

| Target | Input format | Assertions and budgets |
| --- | --- | --- |
| `packets` | Raw packet-line bytes | Four hex digits, cursor progress, exact payload and suffix, non-consuming errors. |
| `responses` | Raw HTTP response bytes | Discovery/HEAD and pack checks, invalid transport length rejection; two 4 KiB response buffers and 128-byte request storage. |
| `routing` | Method byte, capacity byte, path NUL query NUL body | Method selects GET/POST/PUT/DELETE/HEAD; capacity selects 0/1/16/64/4096 bytes. A fixed empty snapshot uses 256-byte tree and 512-byte commit storage, one-byte read scratch and at most 4 KiB output. Successful upload responses must contain NAK and a valid pack when present. |
| `graph` | Mode byte, short-read byte, object record | Even mode mutates a tree; odd mode mutates a commit. Other objects and IDs are rebuilt to reach structural validation. Borrowed/streamed outcomes must match with three metadata/index slots, 64-byte read and 128-byte parse scratch. Short reads range from 1 to 64 bytes. |

Each target ignores inputs over 4096 bytes. Host-side graph construction uses
one bounded `String` plus fixed arrays and borrowed input; the validated core
operates on caller buffers. Each target has its own support module, and the
ordinary mutation runner reuses buffers while preserving its input sequence.
The explicit packet-boundary regression separately exercises 65520-byte frames.
Zlib inflation and pack-entry/delta parsing need additional targets when those
client APIs exist; `pack::validate` currently checks framing and SHA-1 only.

Install the pinned CI tools and run a bounded campaign from the repository root:

```sh
rustup toolchain install nightly-2026-08-09 --component rust-src
cargo install cargo-fuzz --version 0.13.2 --locked
mkdir -p /tmp/git-zero-fuzz/packets
cargo +nightly-2026-08-09 fuzz run packets /tmp/git-zero-fuzz/packets fuzz/corpus/packets -- \
  -runs=5000 -max_len=4096 -timeout=5 -rss_limit_mb=512
```

Repeat for `responses`, `routing` and `graph`, with separate writable output
corpus directories. libFuzzer writes new inputs into the first directory;
the checked-in seed directories stay unchanged. For longer local campaigns,
replace `-runs=5000` with `-max_total_time=600`. CI smoke runs are regression
checks, not a claim of exhaustive coverage.

Replay a crash directly:

```sh
cargo +nightly-2026-08-09 fuzz run packets fuzz/artifacts/packets/crash-HASH
```

Keep a minimized reproducer as a named seed and add a focused assertion if its
expected behavior can be stated precisely. `fuzz/Cargo.lock` pins the optional
harness dependencies. `cargo fmt --manifest-path fuzz/Cargo.toml --check` checks
the standalone package; the normal workspace format and Clippy checks cover the
shared target support.
