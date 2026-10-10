# Contributing

Open a pull request against `main`. Use a Conventional Commit header for its
title and each commit, for example `feat(client): decode pack objects` or
`fix(http): reject truncated requests`. Squash merging uses the PR title.

The default branch requires successful `rust` and `worker` GitHub Actions checks,
resolved review conversations, and a linear history. Force pushes and branch
deletion are disabled. See [tests/README.md](tests/README.md) for local validation
and the parallel interoperability matrix.

The ruleset is recorded in
[.github/repository-ruleset.json](.github/repository-ruleset.json). Repository
administrators apply it through GitHub's ruleset API; editing the file alone
does not change the live settings. The checks are pinned to GitHub Actions
(GitHub App ID `15368`). The ruleset has no administrator bypass, and it requires
a PR without imposing a minimum number of approvals.

## Runnerless

[.runnerless-ci.ts](.runnerless-ci.ts) uses the versioned `runnerless/v1` SDK,
following the Runnerless setup in `pawelchcki/my-infra`. It compiles to Wasm and
checks open PR titles and commit messages. Once the repository is connected,
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
take effect after merging and apply to subsequent PRs. Add its checks to branch
protection after confirming a live, successful run from the My Toolkit App
(GitHub App ID `4602759`).
