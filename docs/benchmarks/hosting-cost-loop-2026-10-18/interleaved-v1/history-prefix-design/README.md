# Proposed historical prefix reads

The [independent design](design.md) reconstructs all 704 measured R5 operation
rows with the current 16 MiB layout. `projection.json.gz` retains each row and
explicit fixed, non-stats and all-observer scopes. `offline-analysis.py` only
reads existing reports and calculates counters; all service and benchmark
execution runners remain Rust.

Private in-memory pack floors and current ref-value stamps can select complete
historical pack closures for narrowly eligible cold v0 fetches. All metadata
shares the existing 32 MiB cache budget; there is no durable sidecar, per-object
cloud storage or hidden full-view cache. External changes, restart and ambiguous
publication discard selectivity. Full metadata validation and every consumed
chunk's ordered identity/hash checks remain mandatory.

Against recorded R5, projected fixed cost falls from $2.33324193618 to
$2.27681193618 (2.4185233%). The final cache-hit recheck leaves a partial view,
so the final global inventory must restore all packs. Charging that restoration
leaves $0.04419 complete physical savings: 1.8576047% against operational
non-stats traffic with restore, or 0.5793006% including all measured observers.
The last scope is below 1%; none of these are measured candidate results.

The design also rules out specific simple alternatives: current full-view read
binpacking is already optimal for every recorded row; catalog replacement with
HEAD plus a durable aggregate increases known cost; enabling multi-pack reuse
alone has no available bitmap to use. These are conditional bounds, not a global
architecture optimum or formal plateau.

A clean implementation is being prepared independently from completed R5
evidence and the verified R8/R9 combined source. Selection and a fresh declared
screen remain gated on reviewing the running R5–R8 screen. It must pass all
unchanged integrity, transfer, storage and latency guards. The final candidate
still requires five fresh original-R1/final pairs. No speed win or complete
cloud bill is established.
