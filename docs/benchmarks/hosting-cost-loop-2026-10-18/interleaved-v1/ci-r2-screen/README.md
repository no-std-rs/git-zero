# R2 bridge batching: isolated two-pair screen

[Measurement run 39081580940](https://github.com/no-std-rs/git-zero/actions/runs/39081580940)
ran all four profiles successfully on separate standard public GitHub-hosted VMs.
Each profile measured fresh scalar R1 and batched R2 providers on the same VM,
with two pairs and one warmup per provider. The services were local workerd,
R2 and SQLite emulation. No paid Cloudflare deployment was used.

The workload has 32 pushes and 56 verified fetches per trial, including seven
large later pushes, latest reads and random older history with and without haves.
The objective models 1,000 repetitions of each of four profiles plus 30 days of
peak R2 storage. Known service cost fell from **$3.28607843618 to
$2.78119243618 (15.3644%)**. All cost and resource guards passed. Cold reads
saved 561 Worker and 561 DO requests per trace; actual R2 GETs, read bytes,
write bytes and stored pack bytes were preserved.

The formal decision remains **inconclusive**. This is a two-pair screen, and
seven latency guards failed; `guard-report.json` retains every measured host/Git
ratio and median/p95 result. In particular, history and push tail stalls remain.
This establishes neither a speed win nor a plateau. Worker CPU, DO duration,
SQLite monthly storage and native compute remain unknown, so full cost is null.
Rates are gross linear service rates without free allowances or subscriptions.

The original CI score step failed because its reader required an explicit
zero-GET counter before the first cold fetch. A strictly validated absent GET
entry now means zero; malformed, failed or missing positive accounting remains
invalid. Only the reader was repaired. The independent evaluation used binary
`01b36df2f5f2019eeb09ebe79eb8ae80e9f86d0cdf39667c9fec9b0584e61fb8`,
built from `386fa67d594bb2ce12776140a66bf03aec931a95`; evaluator provenance records
both old and repaired readers. No measurement was rerun or raw report changed.
The failed scoring log and archive remain included.

Each `hosting-PROFILE.tar.gz` contains that profile's complete reports, service
health, VM/source/binary provenance and an internal checksum manifest. Extract
it into `profiles/PROFILE/`, then run `sha256sum --check RAW_SHA256SUMS` here to
verify all eight original reports and eight health records. First check this
publication with `sha256sum --check SHA256SUMS`. `summary.json` gives the compact
result; `comparison.json` and `guard-report.json` retain all decisions and guards.

`combined-candidate-validation.txt` records correctness checks for the prepared
R4/R5 integration separately from these R2 timings, including a preserved local
runtime timeout and successful retry/restart in a fresh namespace. Those later
candidates have not yet been measured in this publication.
