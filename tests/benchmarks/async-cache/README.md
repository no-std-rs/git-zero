# Native stream baseline check

This small correctness/performance smoke comparison covers the existing native
streaming path while async caching, deadlines and opt-in chunked intake are added.
It does not measure deployed Workers, async-cache hit CPU, WAN behavior or Linux
history. All new host options are disabled for the baseline comparison.

Both versions used Git 2.53.0, the same installed stable Rust compiler, Cargo
`release` (size optimization), the matched stored/no-delta encoder profile,
five paired samples and one warmup. Base is `dd96a56`; head is this PR's code.
Fixture generation was separate because the harness refuses existing fixture
directories; both produced HEAD `c61523503dcaf0f9fc5baa5a9fde1829909d7433`,
27 objects, 23,028 raw bytes and SHA-256
`275a6d86f4c8971d53280786e4f9b788187cb7bab95270b0afbf4a15ac9a3dde`.

| Workload | Base wall median | Head wall median | Base server CPU median | Head server CPU median | Base/head frontend peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Smart clone | 21.220 ms | 16.819 ms | 0.116 ms | 0.090 ms | 2648/2624 KiB |
| Loose dumb clone | 24.324 ms | 17.218 ms | 0.612 ms | 0.414 ms | 2704/2812 KiB |

Raw zero-server observations are in [observations.csv](observations.csv).
Wire response bytes were unchanged: smart 23,934, dumb 23,943. No runtime
regression was observed on this fixture; the independent Git side also became
faster between runs, so the wall-time reduction does not establish a speedup.
Process/startup RSS varies across samples; these small absolute differences do
not establish a memory improvement or regression or provide whole-host accounting.

Reproduce with clean base/head checkouts, separate fixture/output paths, and the
same installed Git/toolchain. Build each checkout with:

```sh
cargo build --release -p git-zero-rustix --bin git-zero --example benchmark-server --locked
python3 tests/benchmark.py --allow-unpinned-git \
  --binary target/release/git-zero \
  --harness target/release/examples/benchmark-server \
  --files 16 --blob-bytes 1024 --commits 4 \
  --case server-clone-smart --case server-clone-dumb \
  --samples 5 --warmup 1 --fixture-dir /tmp/git-zero-unique-fixture \
  --output /tmp/git-zero-unique-observations.json
```

Verify the generated fixture IDs/checksum match before comparing observations.
The harness retains per-sample client/server CPU, RSS, requests and bytes; the
CSV preserves the zero observations without committing generated repositories.
