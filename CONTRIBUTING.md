# Contributing

Collaborators may open a pull request against `main`. Use a Conventional Commit header for its
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

GPU complexity comparisons and the LM-CC/token merge gate are retired. Public
CI runs on GitHub-hosted runners. Private runner pools are reserved for
private repositories.

## Runnerless

[.runnerless-ci.ts](.runnerless-ci.ts) uses the versioned `runnerless/v1` SDK,
following the Runnerless setup in `pawelchcki/my-infra`. It compiles to Wasm and
checks open PR titles and commit messages. This repository is connected, and
the My Toolkit GitHub App publishes `Runnerless / Program execution`,
`Runnerless / PR title`, and `Runnerless / Commit messages`. Policy checks are
regenerated on status and completion deliveries too, so a recovered PR delivery
can fill missing contexts.
The native title declaration and trusted `.ci-toolkit.yml` commit policy also
publish `ci-toolkit/pull-request-title` and `ci-toolkit/commit-messages` independently
of the repository program.
The program retains its legacy policy checks for existing protection compatibility.
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
