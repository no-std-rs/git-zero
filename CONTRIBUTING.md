# Contributing

Open a pull request against `main`. Use a Conventional Commit header for its
title and each commit, for example `feat(client): decode pack objects` or
`fix(http): reject truncated requests`. Squash merging uses the PR title.

The default branch requires successful `rust` and `worker` GitHub Actions checks,
`Runnerless`, `Runnerless / PR title`, and `Runnerless / Commit messages`. Review
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
published comment and refreshes its checksum envelope. Runnerless reads only the
verified comparison publisher's bot comment, matching its repository, PR, head,
and target commit before comparing ratios; it does not use rounded table values.
The gate also requires `buildbuddy.yaml` and the ratio script to match the trusted
base revision. Python runs with `-I` so files in the PR cannot shadow its standard
library imports. Changes to these producer files require a separate trusted
rollout; a PR cannot change the producer and pass its own measurement gate.

Runnerless executes the trusted base program. The new gate starts on PRs whose
base contains this policy; this PR validates the program and report handoff
locally. After the policy lands, apply the recorded ruleset to require
`Runnerless / LM-CC/token` from My Toolkit (`4602759`). Adding the required context
before the base program supports it would leave PRs waiting for a missing check.
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
the My Toolkit GitHub App publishes `Runnerless`, `Runnerless / PR title`, and
`Runnerless / Commit messages`. Rust builds and real Git interoperability tests
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
