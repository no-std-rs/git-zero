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
| `routing` | Method byte, capacity byte, path NUL query NUL body | Method selects GET/POST/PUT/DELETE/HEAD; capacity selects 0/1/16/64/4096 bytes. A fixed empty snapshot uses 256-byte tree and 512-byte commit storage, one-byte read scratch and at most 4 KiB output. Negotiation, sideband and conflicting-capability seeds exercise ACK/NAK and checksum-valid raw/sideband packs. |
| `graph` | Mode byte, short-read byte, object record | Even mode mutates a tree; odd mode mutates a commit. Other objects and IDs are rebuilt to reach structural validation. Borrowed/streamed outcomes must match with three metadata/index slots, 64-byte read and 128-byte parse scratch. Short reads range from 1 to 64 bytes. |
| `inflation` | Raw zlib bytes | Whole/fragmented outcomes, emitted bytes and terminal positions must match. One 32 KiB history window, two 4 KiB staged outputs and 258-byte guarded scratch; 4096/257-byte output budgets with 64/1 expansion ratios. Old history cannot affect a new stream, trailing bytes stay unconsumed, errors stay terminal and each call makes bounded progress. |
| `loose` | Inflated loose bytes | Actual SHA-1 reaches header/length checks; successful bodies must reserialize to the input and reject a wrong ID. |
| `entries` | Pack/delta bytes | Repaired SHA-1/SHA-256 checksums reach entry parsing. Complete and fragmented readers must agree on bodies, headers, offsets, IDs, CRCs and rejection. Two 32 KiB windows, guarded 1 KiB entry/result buffers, 17-byte streamed output, a fixed 256-byte base, 128 delta instructions and depth 2 bound the work. |
| `receive` | Raw receive-pack control bytes | SHA-1/SHA-256 commands, capability selection, options, shallow boundaries and terminal errors. A guarded 1 KiB packet buffer, eight commands/options/boundaries, 4 KiB control budget and guarded 512-byte report output bound the work. |

Routing also runs validated selection and pull responses with shallow/filter
seeds, checking boundary packets, graph budgets and cancellation.

Each target ignores inputs over 4096 bytes. The packet oracle formats a four-byte
header independently and accepts either hex case. Host-side graph construction uses
one bounded `String` plus fixed arrays and borrowed input; the validated core
operates on caller buffers. Each target has its own support module, and the
ordinary mutation runner reuses buffers while preserving its input sequence.
The explicit packet-boundary regression separately exercises 65520-byte frames.
Inflation seeds are generated with Python's zlib and cover stored, fixed,
dynamic, empty, excessive-expansion and bad-checksum streams. Loose seeds cover
all relevant envelope paths; pack seeds include plain entries, both delta kinds,
standalone instructions and reserved types. `pack::validate` retains its original
framing/SHA-1 behavior; `pack::Reader` also checks entry bodies and exact closure.
The `entries` target additionally executes fragmented delta events against a
fixed patterned base and compares acceptance and result bytes with `apply_delta`.

Install the pinned CI tools and run a bounded campaign from the repository root:

```sh
rustup toolchain install nightly-2026-08-09 --component rust-src
cargo install cargo-fuzz --version 0.13.2 --locked
mkdir -p /tmp/git-zero-fuzz/packets
cargo +nightly-2026-08-09 fuzz run packets /tmp/git-zero-fuzz/packets fuzz/corpus/packets -- \
  -runs=5000 -max_len=4096 -timeout=5 -rss_limit_mb=512
```

Repeat for `responses`, `routing`, `graph`, `inflation`, `loose`, `entries` and `receive`, with separate writable output
corpus directories. libFuzzer writes new inputs into the first directory;
the checked-in seed directories stay unchanged. For longer local campaigns,
replace `-runs=5000` with `-max_total_time=600`. CI smoke runs are regression
checks, not a claim of exhaustive coverage.

Replay a crash directly:

```sh
cargo +nightly-2026-08-09 fuzz run packets fuzz/artifacts/packets/crash-HASH
```

Keep a minimized reproducer as a named seed and add a focused assertion if its
expected behavior can be stated precisely. Register its `include_bytes!` entry
in `tests/fuzz.rs` too: ordinary tests embed the seed corpus, so missing fixture
paths fail at compile time and replay does not depend on the working directory.
`fuzz/Cargo.lock` pins the optional
harness dependencies. `cargo fmt --manifest-path fuzz/Cargo.toml --check` checks
the standalone package; the normal workspace format and Clippy checks cover the
shared target support.
