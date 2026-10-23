# Contributing

Open a pull request against `main`. Use a Conventional Commit header for its
title and each commit, for example `feat(client): decode pack objects` or
`fix(http): reject truncated requests`. Squash merging uses the PR title.

The default branch requires successful `rust` and `worker` GitHub Actions checks,
`Runnerless / PR title`, and `Runnerless / Commit messages`. Review
conversations must be resolved, and history must be linear. Force pushes and
branch deletion are disabled. See [tests/README.md](tests/README.md) for local
validation and the parallel interoperability matrix.

The ruleset is recorded in
[.github/repository-ruleset.json](.github/repository-ruleset.json). Repository
administrators apply it through GitHub's ruleset API; editing the file alone
does not change the live settings. Rust/Worker checks are pinned to GitHub Actions
(GitHub App ID `15368`); Runnerless checks are pinned to My Toolkit (`4602759`).
The ruleset has no administrator bypass, and it requires a PR without imposing
a minimum number of approvals.

Auto-merge is available after required checks pass. Squash and rebase merges are
enabled, and GitHub deletes merged branches automatically.

## Test repositories

Generate repository fixtures during tests, or download upstream repositories on
demand and pin their full commit IDs. Keep datasets in `/tmp` or the ignored
`test-repositories/` directory. Commit fixture generators and download instructions
rather than repository clones, packs, bundles or exported snapshots.

## Complexity reports

`buildbuddy.yaml` runs the advisory [llm-cc comparison](https://github.com/pawelchcki/llm-cc/blob/main/tools/comparison/README.md)
on PR branch updates targeting any branch, and on `main` pushes to refresh the
repository baseline. The shared Bazzite host prepares a comparison against the
PR's actual target and schedules GPU inference only for uncached source blobs.
When `Complexity comparison` succeeds, Runnerless uses
[.runnerless-ci.ts](.runnerless-ci.ts) to validate llm-cc's `publication.json` and
post the generated `comment.md` in the GitHub PR conversation through the
My Toolkit bot. Branch updates refresh the same comment, including for PRs
targeting another feature branch. Pushes to `main` publish baseline files without
a PR comment. Raw complexity totals remain informational: adding code may increase
the total while reducing complexity per token.

`Runnerless / LM-CC/token` compares the repository-wide total LM-CC divided by
total token count against the PR's merge base. Equal or lower ratios pass; any
increase fails. Missing, incomplete, non-finite, zero-token, or stale measurements
fail. `scripts/llm-cc-ratio.py` adds full-precision ratios from `report.json` to the
published comment and refreshes its checksum envelope. Runnerless performs one indexed lookup for the current comparison publisher's
bot comment, without enumerating PR comment history. It matches repository, PR, head,
and target commit before comparing ratios; it does not use rounded table values.
The gate also requires `buildbuddy.yaml` and the ratio script to match the trusted
base revision. Python runs with `-I` so files in the PR cannot shadow its standard
library imports. Changes to these producer files require a separate trusted
rollout; a PR cannot change the producer and pass its own measurement gate.

Runnerless executes the trusted base program. Apply the recorded ruleset only
after its additional `Runnerless / Program execution` and `Runnerless / LM-CC/token`
contexts have been verified on an open PR. Both belong to My Toolkit (`4602759`).
Requiring a context before the base program supports it leaves PRs waiting for
a missing check. The obsolete bare `Runnerless` context is not emitted.
Apply the recorded strict status-check setting at the same time. If the target
branch advances, GitHub requires the PR branch to be updated before merging,
which gives the new head a fresh comparison rather than accepting an older pass.

`.llm-cc/rules.json` classifies the core and native adapters as runtime, test
and fuzz directories as tests, and demos/scripts as tooling. Generated build output,
dependencies, and vendored code are excluded. Validate changes with
`llm-cc rules check .llm-cc/rules.json` and `llm-cc rules explain PATH...`.

To compare any local committed branch, including one without a PR:

```sh
git fetch origin main
llm-cc compare run --target origin/main --output-dir /tmp/git-zero-comparison
# Compare another local branch, or use a different target for a stacked PR:
llm-cc compare run --head feature-branch --target parent-branch --output-dir /tmp/git-zero-comparison
```

Read `report.md` in the output directory. Local runs require an installed
llm-cc and a model/backend; `llm-cc doctor --format json` checks prepared assets.
Comparisons use committed Git objects. On a CPU-only machine, add
`--force-cpu --model-name qwen2.5-coder-3b-q6_k` and prepare that model first.

Automatic runs require the repository to be linked in BuildBuddy with access to
the `linux-amd64-kvm` pool and the `bazzite-host` resource, and the prepared
`/var/lib/llm-cc/bin/llm-cc-coordinate` launcher. Both GitHub apps have
organization-wide access. Configure a `GITHUB_TOKEN` BuildBuddy group secret
for authenticated discovery. Runnerless publication and comparison rules come
from the PR's target commit, so these files must land on the target branch before
subsequent PRs use them. The BuildBuddy comparison supplies the report; the
Runnerless ratio check supplies the merge decision.

## Runnerless

[.runnerless-ci.ts](.runnerless-ci.ts) uses the versioned `runnerless/v1` SDK,
following the Runnerless setup in `pawelchcki/my-infra`. It compiles to Wasm and
checks open PR titles and commit messages. This repository is connected, and
the My Toolkit GitHub App publishes `Runnerless / Program execution`,
`Runnerless / PR title`, and `Runnerless / Commit messages`. Policy checks are
regenerated on status and completion deliveries too, so a recovered PR delivery
can fill missing contexts. A stale canonical comparison fails the ratio gate
immediately while waiting for its publisher to update the same comment.
The native title declaration and trusted `.ci-toolkit.yml` commit policy also
publish `ci-toolkit/pull-request-title` and `ci-toolkit/commit-messages` independently
of the repository program, so a failed comparison lookup cannot starve them.
The program retains its legacy policy checks for existing protection compatibility.
The indexed lookup requires the Runnerless host released by my-infra PR #296;
unknown legacy comment bindings remain unavailable until the publisher refreshes them.
Rust builds and real Git interoperability tests
run in the existing GitHub Actions workflow.

An organization owner or admin connects `no-std-rs/git-zero` in
[the Runnerless application](https://app.runnerlesshq.com/app/repositories).
The existing My Toolkit installation must have repository access and its GitHub
account must be bound to the chosen Runnerless organization. Connecting verifies
the repository and installation IDs; it needs no repository secret or separate
infrastructure allowlist. Repository automation settings require a recent sign-in.

Runnerless reads PR programs from the trusted base revision. Changes to this file
take effect after merging and apply to subsequent PRs. All three Runnerless checks
were verified on a live PR before being added to protection.

## Crate boundaries and releases

For subsystem entrypoints and their upstream Git equivalents, start with the
[source map](docs/upstream-mapping.md).

Keep shared object and wire types in `git-zero-core`. Client and server crates
must depend on the core rather than on one another. New decoding/validation APIs
belong behind `read`; snapshot construction and encoding APIs belong behind
`write`. Keep default-disabled builds warning-free and allocation-free.

Run `python3 scripts/check-features.py` to compile each feature combination
independently; a workspace-wide build can unify features and hide missing gates.
Run `python3 scripts/check-public-packages.py` to verify publishable metadata and
dependency ordering. Release helper tests run with the existing Python suite.
See the [release guide](docs/releases.md) for the initial history boundary and
the publishing lifecycle.
