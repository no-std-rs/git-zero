# Historical Linux v6.12 snapshot benchmark

These are preserved observations of git-zero revision
`f71e2669f3dbf381af785ac35d49ebbff708a001`, using pinned Git 2.53.0 and Rust
1.97.1. They do not measure the later documentation base `73589c0` or subsequent
revisions. No benchmark was rerun to publish this note.

The dataset retains Linux v6.12's exact upstream tree in one derived parentless
commit: **91,865 objects, 1,476,536,879 raw object bytes and 86,680 checkout
entries**. Linux history is excluded. The [snapshot provenance](snapshot-provenance.json)
records the upstream tag, commit and tree IDs and the standalone snapshot ID;
the [paired fixture proof](paired-fixture-proof.json) verifies identical canonical
bodies across profiles. The annotated tag's signature was not independently
authenticated; exact content identity is the recorded claim.

## Method and clone observations

An identical Git client clones from Git's HTTP backend and git-zero over warm
loopback smart HTTP v0. Each profile has one warmup and three measured clones
per server, in paired Git/zero/zero/Git order. Profiles ran sequentially, matched
first. Fixture preparation, server startup and subsequent correctness checks
are outside clone timing. Server CPU includes frontend and waited backend-child
CPU; client CPU is separate and can overlap transport work.

The release profile uses `opt-level=s`, LTO, one codegen unit and `panic=abort`.
The host has an AMD Ryzen 9 7900, 24 available logical CPUs, approximately 125 GiB
RAM and local NVMe. [Environment](environment.json) and the raw observations
retain versions, affinity, load, limits and source/binary fingerprints.

| Policy | Server | Clone median (range), seconds | Server CPU, seconds | Client CPU, seconds | HTTP bodies, decimal MB |
| --- | --- | ---: | ---: | ---: | ---: |
| Stored DEFLATE, no deltas | Git | 15.67 (15.55–16.22) | 6.69 | 11.75 | 1478.74 |
| Stored DEFLATE, no deltas | git-zero | 12.82 (12.45–15.25) | 2.12 | 13.81 | 1485.52 |
| Git defaults versus stored git-zero | Git | 27.57 (27.16–27.82) | 41.44 | 9.20 | 270.16 |
| Git defaults versus stored git-zero | git-zero | 11.67 (11.64–11.99) | 1.79 | 13.30 | 1485.52 |

In the matched profile, git-zero's observed median elapsed time was 18.2% lower
and server CPU 68.3% lower, while its Git client used more CPU. Against Git
defaults, git-zero transferred **5.50 times** as many HTTP body bytes. Its stored
DEFLATE output has no deltas. These small warm-loopback samples do not establish
internet download performance or a general latency ranking; the two profiles
also ran at different times and retain their host-load observations.

Raw data: [matched](kernel-matched.json), [Git defaults](kernel-upstream-defaults.json)
and the original [summary](summary.json).

## Separate download, indexing and checkout experiment

There are two samples per server. Python urllib writes the downloaded pack to
the page cache without fsync, followed by one-thread Git `index-pack` and a
separate checkout. This request negotiates neither sideband nor ofs-delta,
unlike normal clone. Its framing and indexing policy can therefore differ.
These sequential phases are neither additive predictions nor an exact
decomposition of ordinary clone, which overlaps transport and indexing.

| Policy | Server | Download, seconds | Git index-pack, seconds | Checkout, seconds | Pack, decimal MB |
| --- | --- | ---: | ---: | ---: | ---: |
| matched | Git | 6.65 | 5.62 | 4.32 | 1477.84 |
| matched | git-zero | 3.66 | 5.65 | 3.72 | 1478.30 |
| Git defaults | Git | 20.73 | 4.66 | 4.63 | 270.13 |
| Git defaults | git-zero | 3.18 | 5.65 | 3.88 | 1478.30 |

## Separate direct decoder observations

An external temporary Rust host calls the unchanged public pack Reader API
from the same revision and release profile. It loads a complete pack, verifies
its checksum, inflates entries, resolves deltas and checks all canonical IDs,
kinds and sizes against the independently verified manifest. Three samples per
input ran in paired order with warm input page cache and no separate warmups.
All six passed. The host uses manifest-derived storage/inflate/delta bounds;
its observations retain those limits and exact staging/result capacities.

| Input to git-zero decoder | Elapsed median (range), seconds | CPU, seconds | Peak RSS median, GiB | Deltas |
| --- | ---: | ---: | ---: | ---: |
| git-zero stored pack | 3.43 (3.40–3.43) | 3.41 | 2.79 | 0 |
| Git compressed pack | 8.69 (8.67–8.74) | 8.66 | 1.72 | 8,018 |

The compressed pack has maximum delta depth seven. These absolute observations
include process startup, file reads, allocations and canonical verification.
The host retains compressed input, inflated entries and reconstructed delta
bodies; metadata also contributes to RSS. It performs no filesystem checkout or
Git index construction, so these times are not equivalent-work speed ratios
against Git `index-pack`. Shared dependency versions/checksums matched the
measured revision's lockfile. [Decoder samples and summaries](decoder.json)
retain the probe hashes; its earlier small stored/reference-delta/offset-delta
smoke checks are external evidence, not committed tests in this bundle.

## Memory and correctness boundaries

Server memory is sampled aggregate process-tree RSS at 50 ms intervals. Samples
can miss peaks and count shared mappings in multiple processes. Client RSS is
GNU time's largest waited-process lifetime peak, rather than simultaneous
process-tree RSS. These measures must not be added.

| Policy | Server | Server sampled peak, GiB | Client largest-process peak median, GiB |
| --- | --- | ---: | ---: |
| matched | Git | 1.43 | 1.41 |
| matched | git-zero | 1.40 | 1.41 |
| Git defaults | Git | 0.60 | 0.42 |
| Git defaults | git-zero | 1.40 | 1.41 |

The shared benchmark frontend buffers a complete response before sending. Its
memory use is not production streaming-memory evidence. Every warmup, measured
clone and separate phase sample passed strict Git fsck, exact reachable object
ID/body checks, and full tree/mode/symlink/checkout-byte comparison. Fixtures were
independently read back. The [independent completeness audit](independent-validation-audit.json)
records eight clone attempts and four phase attempts per profile, all 12 complete
sampler observations, plus six verified decoder samples. No failed or discarded
timed attempt is omitted from these runs. Agent builds, scoring and other
benchmarks were held during measurement.

## Evidence preservation and reproduction limits

[manifest.json](manifest.json) lists the size, SHA-256 and original local path
of each of the eight JSON artifacts copied byte for byte. No raw fields were
omitted or rewritten. Original absolute `/tmp` paths, loopback URLs and process
IDs remain historical data; they are not portable links to committed assets.
The summary's original evidence paths are likewise retained, with the relative
links above identifying the copies in this directory.

The experiment driver, temporary decoder probe source/manifests/binaries, built
Git tools, full object manifests, downloaded packs, dataset and clone directories
are **not committed here**. The preservation manifest and raw observations
identify their historical hashes and paths. This bundle is preserved evidence,
not a turnkey reproducer or a newly committed automated test. Reproduction
requires recovering those external tools and dataset, following their recorded
fixture preparation, and matching the raw method/toolchain/limits; the committed
benchmark interfaces alone do not implement all separate experiments above.

Full Linux history and incremental fetch, controlled cold-cache, WAN,
concurrent-load and deployed-service measurements remain open. This historical
snapshot does not complete the [kernel benchmark roadmap item](../../../ROADMAP.md)
or establish performance for later revisions.
