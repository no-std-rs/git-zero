# Repository host: fifth-review verification

This covers the fifth review of PR #66 against `cf3655e9`. The current
[contract](repository-host-v1.md) supersedes resource assumptions in the
[fourth-review record](repository-host-review4.md). No deployment was run.

## Findings and regressions

| Item | Result and regression |
| --- | --- |
| 1 | A namespace holds at most codecSlots - 2 heavy arenas, even when alone; namespaces with no heavy lease precede incumbents. Metadata remains protected, without preemption. `admission.test.mjs` holds A's maximum, queues another incumbent, then requires B's clone and push within one second. Existing 300-browse-read fairness remains. |
| 2 | A conservative snapshot catalog aggregate reserves cache capacity before preparation. Entry/total/count exhaustion goes directly to ordinary generation. `shared-pack.test.mjs` requires exactly one/three preparations for one/three uncacheable clones, and one preparation when a new key encounters a full cache. |
| 3 | Explicit token pools validate the fair working allowance. Default token mode has no isolate-wide memory bound, plus up to four idle codecs/factory. `host-module.test.mjs` rejects 64/96/99 MiB, fetches at exactly 100 MiB after reopening, and pushes/clones at 101 MiB. `concurrent-git.test.mjs` pauses A's actual pack until B's pack starts, then verifies overlap, checkout and fsck. |
| 4 | An orphan builder with no output for a response-throughput window is aborted; uncacheable requests never start a builder. Gateway defaults cap pushes at 256 MiB and codec packs at 1 GiB. `shared-pack.test.mjs` abandons all followers before output, checks abort within a scaled 50 ms window/150 ms cleanup bound; `host-module.test.mjs` checks caps and unchanged token defaults. |
| 5 | Mid-stream shared failures reach followers as fatal channel-3 retry messages. Readers withhold incomplete pkt-lines; raw non-sideband requests bypass sharing. `shared-pack.test.mjs` ends an R2 chunk inside a packet, then requires valid fatal framing. `shared-failure-git.test.mjs` injects a second-chunk PUT failure and checks real Git's remote retry message without framing/disconnect diagnostics. |
| 6 | Budgets use an independent oracle: Git selects the twenty commits/root/path trees/blob; stored delta depths are read before the request. API work cannot enlarge the bound. `browse-scale.test.mjs` requires observed work to equal that reference, SQL < 24 + 5 × reference work, zero writes, and tight reference-adjusted 1k-to-25k growth. |
| 7 | Workerd memory and 25k full-clone preparation were repeated with `workerd-memory.test.mjs` and `clone-scale.test.mjs`; figures below. |

Eight targeted regressions copied into an isolated archive of `cf3655e9` all
failed there: heavy admission, token pool, default caps, one/three uncacheable
walks, abandoned builder, partial-frame fatal packet and real-Git fatal error.
They pass with the fixes. Item 6 strengthens the test oracle; ordinary baseline
read costs were already correct and remain unchanged.

## Bounds and decisions

Default heavy limits are two/namespace and three/isolate, with a fourth arena
for bounded metadata. Admission still requires working bytes: raised/legacy
profiles and several saturated namespaces may time out without preempting anyone.
Fairness is **per repository**, not per team. A team with many namespaces gets
many turns. Admission-group keys are not implemented; gateways requiring team
limits must enforce them. Authorization remains namespace-grant based.

At four arenas/13 MiB headroom, a token fetch needs **100 MiB = 24 + 13 + 63**.
A push adds four times its actual control prefix; the small fixture runs at
101 MiB. At 112 MiB this legacy cache profile still fits only one heavy fetch;
lower caches for concurrency. Without an explicit pool there is no isolate-wide
active-codec bound. Four retained idle token codecs/factory cost up to a
**24 MiB allowance / 22.75 MiB actual pinned-codec linear memory**.

At the default 16 KiB/s floor, 256 MiB permits **4.55 hours** of pack input time;
1 GiB permits **18.20 hours** of output time, plus initial credit and separately
bounded controls/admission. CPU/storage/cleanup have their own work/progress/I/O
bounds. Raising a cap to 64 GiB permits roughly **48.5 days** at that floor.
A 300 MB push needs a raised push cap, then can finish at 10 Mbit/s (~240 s)
without an absolute transfer lifetime.

The cache-size aggregate includes duplicate/unreachable published representations
conservatively; it can bypass caching a fitting selected pack. It performs no
graph traversal, object reads or repository-sized JS allocation. Capacity refusal
then pays only the request's ordinary preparation.

An orphan producing regular output may finish a cacheable entry. With no output
for a window it is cancelled, including during preparation/admission; leases
retire after bounded outstanding I/O and cleanup. A mid-stream shared failure
still fails all followers, now with a Git-visible reason to retry. There is no
transparent retry/resume. This is the documented failure contract.

## Measurements

Two held clones plus another incumbent waiter did not block the co-tenant clone
and push: **7.74 ms** in a focused run, under the 1000 ms assertion. The existing
300-read browse fairness workload had a **19.69 ms** light-tenant maximum and
cancelled neither clone. These are local admission measurements, not a CPU/WAN SLA.

The default-packed fixture has **25,000 commits / 150,028 stored objects**,
normal Git deltas, fan-out/depth three and retained rewritten history. Selection:
**17.979 s**; preparation: **2.753 s**; total: **20.732 s**; headers: **20.747 s**.
Strict Git import and object-set comparison verified exactly **150,025 reachable
objects** in **11,774,780 wire bytes**. Templates compiled 21 times/537,840 hits.
The preceding review measured 26.043 s total: this run is **5.311 s faster**.
The 30 s setting is a **lack-of-progress deadline**, not a total cutoff. These
local samples do not establish a causal performance improvement.

Ref-rooted commit/history20/tree/blob at 25k remain **14/70/70/76 SQL statements,
zero writes**. Independent reference delta-work is **1/20/22/32** (1k:
1/20/64/95). Raw proof still completes in 63 batches; signed browser navigation
needs zero building retries.

The workerd memory workload retains two uncached generation streams and thirty
cached readers, then performs one hundred further cache-hit admissions. Ledger
peak: **60.94 MiB / 64 MiB**; three retained arenas (18 MiB allowance), using
**17.0625 MiB actual Wasm linear memory** throughout.

| Phase | JS used heap | JS total heap | Backing storage | Embedder heap |
| --- | ---: | ---: | ---: | ---: |
| Two live generation streams | 21.79 MiB | 29.00 MiB | 7.15 MiB | 8.61 MiB |
| Thirty cached readers added | 22.57 MiB | 29.75 MiB | 8.79 MiB | 8.98 MiB |
| After 100 cache-hit admissions | 9.97 MiB | 17.50 MiB | 31.15 MiB | 1.56 MiB |
| After GC, streams remain live | 3.02 MiB | 4.25 MiB | 5.04 MiB | 0.26 MiB |

These are inspector samples, not RSS or a measured worst-case peak. Product
Rust/Wasm, VM/compiled code, HTTP and SQL still need a combined memory test.

Sixteen-clone regressions passed on both runtimes: Wrangler single/sixteen
**18.193 / 18.901 s** (1.04×), browse **21 ms**, push **44 ms**; direct workerd
**17.521 / 17.723 s** (1.01×), browse **13 ms**, push **53 ms**. Each group used
one build and checked snapshots, ref races and deletion under live reads. Token
clones use 16 MiB incompressible content/independent throttled links, retain the
original 30-second assertion, and pause the actual pack, not v2 ls-refs.

## Verification

Locked npm installation, focused regressions, independent browse scale, both
runtime clone workloads and refreshed measurements passed. The full local host suite passed **260/260 in 406.141 s**, with zero failures,
skips or cancellations; the strengthened three-blob admission test also passed
its focused rerun. Required-check results are recorded in the PR description. Rust
workspace tests, **96 Python tests**, **39 warning-denied isolated feature
builds** and public-package checks passed. No Rust source, dependency, existing
deadline, SQL batching assertion, lint or fuzz target was weakened.
The uncached scratch/publication regression uses five arenas with its existing
small-cache profile: two held encoders plus a push need three heavy leases in
that namespace. Its workload, one-second response assertions and ten-second
test deadline remain unchanged; the default now deliberately allows two heavy
leases per namespace to protect a co-tenant.

Generated archives/repositories/packs stay under ignored `.scratch/` and are
deleted after measurement; only small logs remain. No other checkout or process
command line/environment was inspected. Historical timeout tracking remains both
runtime tree-depth tests; old reset reports were Wrangler tree-depth and direct
workerd wanted-closure. Their exact cause remains unproven; original tests remain.
