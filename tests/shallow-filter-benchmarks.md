# Shallow/filter regression measurements

Compare baseline `674d33cb75ad2f1c985fce145defd1172bede9da` with this shallow/filter
change using the existing release benchmark, Git 2.53.0, 15 alternating paired
samples per implementation, three warmups and one workload worker. Both profiles
run all ten existing workloads; every object/ref/checkout check and configured
resource limit passed. Fixtures have the same raw fingerprint, 111 objects and
1,341,030 raw bytes. Raw reports retain binary SHA-256 values, observations,
server CPU/RSS, startup costs, wire bytes and the dirty-tree source marker.

Median git-zero client wall times in milliseconds (server-clone rows measure the
real Git client against git-zero):

| Policy | Workload | Main | Change |
| --- | --- | ---: | ---: |
| matched | Loose read | 3.414 | 3.178 |
| matched | Pack read | 4.905 | 4.759 |
| matched | Dumb clone | 41.409 | 41.323 |
| matched | Smart clone | 26.698 | 26.058 |
| defaults | Loose read | 4.506 | 4.788 |
| defaults | Pack read | 4.599 | 5.017 |
| defaults | Dumb clone | 41.651 | 41.672 |
| defaults | Smart clone | 25.835 | 25.984 |

An initial 15-sample comparison showed smart-clone server CPU increasing from
1.431 to 1.551 ms under the matched policy (1.429 to 1.555 ms under defaults).
A fresh 31-pair matched confirmation using saved baseline binaries measured
1.606 → 1.586 ms server CPU and 26.455 → 25.682 ms client wall time. The initial
increase did not reproduce. Small local-read/process-startup changes also moved
with Git's control observations. These warm runs found no reproducible regression;
they are not a statistical guarantee or a cold-cache measurement.

Commands for each revision (save its binaries before rebuilding the other):

```sh
cargo build -p git-zero-rustix --bin git-zero --example benchmark-server --example decoder-read --release --locked
python3 tests/benchmark.py --samples 15 --warmup 3 --jobs 1 \
  --limits tests/benchmark-limits.json --output /tmp/benchmark-matched.json
python3 tests/benchmark.py --git-profile upstream-defaults --samples 15 --warmup 3 --jobs 1 \
  --limits tests/benchmark-limits.json --output /tmp/benchmark-defaults.json
python3 tests/benchmark.py --case server-clone-smart --samples 31 --warmup 3 \
  --output /tmp/benchmark-smart-confirm.json
```

These existing HTTP timings exercise the benchmark host's complete-pack route;
new shallow/filter correctness comes from the separate selected-fetch Git oracle,
allocation tests, seeded fuzz mutations and Worker memory tests. The Worker keeps
the same 24-page initial ceiling and never grows memory during extended requests,
fragmented pulls, cancellation, restarts and independent instances.
