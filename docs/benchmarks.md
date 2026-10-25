# Runnerless benchmarks

The [Benchmarks workflow](../.github/workflows/benchmarks.yml) runs on each push
to main and on PR creation, updates and reopening, including forks. It builds
release binaries and the version in `tests/git-version.txt`, then measures four
smart-HTTP workloads sequentially: client discovery, server discovery, server
clone and server fetch. Each has 25 measured pairs after three warmup pairs,
using 64 files of 16 KiB and 16 commits. Both variants use matched stored DEFLATE
and no deltas. The original report retains every client and server measurement,
binary hashes, fixture, algorithm policy and host metadata.

Runnerless displays client-command wall time, CPU and largest-process peak RSS.
Server measurements remain in the original JSON. Servers and OS caches stay
warm, and order alternates deterministically. The reports are descriptive and
do not claim independent experiments, significance, cold-cache performance or
equivalence to Git's default compression policy. Measurements are advisory and
do not introduce a performance merge gate.

## Add more main data

Select **Actions → Benchmarks → Run workflow → main**, or run:

```sh
gh workflow run benchmarks.yml --repo no-std-rs/git-zero --ref main
```

Rerunning an existing producer job also creates a distinct dataset, keyed by
GitHub run ID and attempt. Benchmark and publisher jobs have no concurrency
group that could cancel pending runs. Runnerless retains all datasets; the
README shows the newest main run by run ID and attempt, even when uploads arrive
out of order. Repeating a run adds evidence in history; observations from
different GitHub machines are kept as separate experiments.

## Retention and reports

Runnerless keeps **all main raw measurements and reports indefinitely**. PR raw
measurements and reports live while the PR is open and for **six calendar months
after closure or merge**. Reopening cancels the expiry until the next close.
A close event runs a small lifecycle job so the trusted publisher can synchronize
the PR's current state. Delayed deliveries cannot undo a newer reopen.

The Runnerless application's repository **Benchmarks → CI benchmark history**
shows every retained run, with pagination, its comparison and a download of all
original measurements. Existing `benchmarks:read` grants also authorize:

```text
GET /api/v1/orgs/ORG/repositories/REPO/benchmark-ci/runs
GET /api/v1/orgs/ORG/repositories/REPO/benchmark-ci/runs?pull=0
GET /api/v1/orgs/ORG/repositories/REPO/benchmark-ci/runs?pull=PR_NUMBER
GET /api/v1/orgs/ORG/repositories/REPO/benchmark-ci/runs?before=RUN_ID-ATTEMPT
GET /api/v1/orgs/ORG/repositories/REPO/benchmark-ci/runs/RUN_ID-ATTEMPT
```

`pull=0` selects main. History orders by run ID/attempt and returns `next_cursor`.
The raw endpoint includes the exact report's original observations and the
Runnerless comparison. These downloads require repository access.

Public JSON and SVG reports are served under
`https://app.runnerlesshq.com/api/public/benchmark-ci/no-std-rs/git-zero/`:

| Suffix | Content |
| --- | --- |
| `main` | Latest main comparison JSON |
| `main.svg` | Full latest main comparison card |
| `badge.svg` | Latest main client-discovery wall-time badge |
| `RUN_ID-ATTEMPT` | An immutable run's comparison JSON |
| `RUN_ID-ATTEMPT/report.svg` | That run's SVG report |

Each current-head PR run gets a comment with its commit, comparison table and
report links. Retrying publication updates that run's own bot comment. A newer
producer attempt gets its own comment, so concurrent publications cannot
overwrite one another. Superseded-head runs remain in Runnerless history.

## Publication trust and recovery

The measurement job has read-only repository permissions. The
[publisher](../.github/workflows/benchmarks-publish.yml) runs separately through
`workflow_run`, from the workflow's immutable default-branch commit. It only
reads a bounded `benchmark.json` ZIP member, without extracting or executing
artifact files. It verifies the producer path, exact run attempt and source
commit through GitHub's API. Fork PRs never receive the publication credential.

The publisher uses the GitHub `benchmarks` environment and a short-lived OIDC
token with audience `https://app.runnerlesshq.com`. Runnerless verifies GitHub's
signature, connected immutable repository ID, main ref, exact publisher workflow,
environment and event. The live main-branch
[public opt-in](../.runnerless-benchmarks.json) authorizes submission and public
reports. Removing it or setting `public` to false stops new submissions and public
reads; retained private data keeps its lifecycle.

Failed benchmark reports retain successful measurements and explicit failures.
Builds that fail before producing measurements have no report to upload.
Malformed or oversized data fails explicitly. Service outages fail the publisher
job. GitHub keeps its source artifact for 90 days, while its supported workflow
rerun window is 30 days; within that window, rerun the publisher job to retry the
same immutable dataset. Rerun **Benchmarks** to collect new data.

The Runnerless implementation was initially deployed from `pawelchcki/my-infra` revision
`4ba59b7c6372a9f7b9220611791a3777f5499d80`. Migration
`0029_ci_benchmarks.sql` was applied on 2026-10-25, and the production submission
endpoint was verified to reach OIDC authentication with no pending migrations.
The repository must already be connected to an active Runnerless organization. If
the GitHub `benchmarks` environment has protection rules, its approvals also apply
to the trusted publisher. The first post-merge main run populates the README
images; pre-activation images return unavailable rather than fabricated results.
