# Releases

Runnerless plans versions and publishes the five public crates. GitHub Actions
builds and verifies package uploads using the existing release workflow.
`version.txt` is the release authority; source manifests retain their baseline
versions until `scripts/prepare-release.py` stages a packaging checkout.

## Initial history boundary

The initial `version.txt` contains `0.1.0` and was introduced on `main` by
`7023b93b3abc6860960bad6fa6acdb3807236a84` (PR #21). Runnerless requires an
existing `v0.1.0` tag to delimit subsequent conventional commits. Without it,
the Release check reports `missing_tag` and cannot propose a release PR.

Use that merged ancestor as the initial history boundary, rather than a feature
branch or a later development revision. Verify it before seeding the tag:

```sh
git fetch origin main --tags
git log --oneline origin/main -- version.txt
git merge-base --is-ancestor 7023b93b3abc6860960bad6fa6acdb3807236a84 origin/main
git show 7023b93b3abc6860960bad6fa6acdb3807236a84:version.txt
```

The baseline tag is distinct from a published GitHub Release or crates.io
version. Creating a published GitHub Release triggers package preparation and
publication. Never move an existing release tag or overwrite package assets.

## Normal release flow

After bootstrap, default-branch changes let Runnerless propose a release PR
updating `version.txt` and `CHANGELOG.md`. A merged release PR is finalized into
its immutable tag and published GitHub Release.

`.github/workflows/publish-release.yml` checks out that exact tag, stages public
manifest/dependency versions and runs package verification. It captures Cargo
upload bodies locally, then attaches them and `SHA256SUMS` to the release.
Runnerless verifies the matching stable release and assets before uploading
`git-zero-core`, `git-zero-client`, `git-zero-server`, `git-zero` and
`git-zero-rustix`, in that dependency order. Registry credentials stay in the
configured Runnerless publishing destination.

## Local validation

```sh
python3 -m unittest discover -s tests -p 'test_release*.py'
python3 scripts/check-public-packages.py --release
```

To stage another version, use a clean temporary checkout. Run
`python3 scripts/prepare-release.py vMAJOR.MINOR.PATCH` only when that version
matches its `version.txt`; the command edits manifests and `Cargo.lock`.
