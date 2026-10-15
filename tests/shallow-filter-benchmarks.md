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
| matched | Loose read | 3.414 | 3.373 |
| matched | Pack read | 4.905 | 4.531 |
| matched | Dumb clone | 41.409 | 43.321 |
| matched | Smart clone | 26.698 | 26.662 |
| defaults | Loose read | 4.506 | 4.506 |
| defaults | Pack read | 4.599 | 4.605 |
| defaults | Dumb clone | 41.651 | 41.453 |
| defaults | Smart clone | 25.835 | 25.633 |

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

The final complete-pack rerun measured matched smart-clone server CPU at
1.448 ms (baseline 1.431 ms); upstream-default CPU was 1.434 ms (1.429 ms).
The matched dumb-clone wall increase also appeared in the Git control
(42.481 → 43.375 ms); it was absent in the upstream-default and selected runs.

An additional `--zero-selection` run exercises requested-closure serving through
the same ten workloads. The baseline checkout uses **only the identical benchmark
host and runner instrumentation** from this change, against the unchanged main
core/server libraries. Build that checkout into a separate target directory and
supply its saved binaries with `--harness`, `--binary` and `--decoder`. Both reports
record the instrumentation as dirty; this does not claim an unmodified historical
benchmark host supported the new flag. With the same 15 pairs and three warmups:

| Selected smart clone, matched | Main | Change |
| --- | ---: | ---: |
| Client wall, ms | 25.678 | 25.735 |
| Server CPU, ms | 1.676 | 1.725 |
| Frontend peak RSS, KiB | 3920 | 3944 |
| Backend child peak RSS, KiB | 0 | 0 |
| Validation metadata, bytes | 6216 | 7104 |
| Selection metadata, bytes | 6216 | 7104 |

A later 31-pair affinity-controlled confirmation exposed a selected-path CPU
increase (1.580 → 1.762 ms), although the complete-pack route stayed steady.
Binary-search metadata lookup copied the larger record on every probe. Returning
a borrowed record removed that cost: the final controlled confirmation measured
1.580 → 1.557 ms server CPU and 26.628 → 26.663 ms client wall time. Both use
`taskset -c 2`, recorded in the reports' affinity metadata. This final result,
together with the complete-pack repeats, found no reproducible regression.

Each metadata array grows by eight bytes per object on this 64-bit host to retain
bounded-history state. Other selected buffers stay at 2112 want bytes, 888 index
bytes, 4096 parse bytes and a 64 MiB graph-read limit. The object-read scratch is
shared with ordinary serving. Both selected runs passed all object/ref/checkout
and resource checks. The change advertises 15 additional bytes for shallow/filter
capabilities; ordinary full-clone pack contents remain identical.

```sh
python3 tests/benchmark.py --zero-selection --samples 15 --warmup 3 --jobs 1 \
  --limits tests/benchmark-limits.json --output /tmp/benchmark-selected.json
```

Shallow/filter correctness also comes from the independent Git oracle, allocation
tests, seeded fuzz mutations and Worker memory tests. The Worker keeps the same
24-page initial ceiling and never grows memory during extended requests,
fragmented pulls, cancellation, restarts and independent instances.
