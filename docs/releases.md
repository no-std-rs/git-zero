# Releases

Runnerless plans versions and publishes the five public crates. GitHub Actions
builds and verifies package uploads using the existing release workflow.
`version.txt` is the release authority; source manifests retain their baseline
versions until `scripts/prepare-release.py` stages a packaging checkout.

## Package family

The public names are `git-zero`, `git-zero-core`, `git-zero-client`,
`git-zero-server`, and `git-zero-rustix`. Rust imports replace hyphens with
underscores, for example `git_zero`. The native executable is `git-zero`,
installed from `git-zero-rustix`; the facade is a library package.

All five packages use one release version. Examples, the kernel codec, and Wasm
hosts have `publish = false`. The Rust libraries are allocation-free; the CLI
process wrapper and JavaScript host applications have separate runtime budgets.

## First publication

`version.txt` starts at `0.1.0`. The first published release is `v0.1.0`, tagged
at the reviewed publication-ready commit on `main`. Check that all five package
names are available before the first upload; the registry allocates names on
first publication.

Merge publication metadata, documentation, and workflow changes before creating
the release. Verify the exact commit, clean review, and CI results, then publish
the GitHub Release for that commit and matching version. A published release
starts the package workflow and can upload all five crates through Runnerless.
It is a publication action, not a packaging dry run.

Runnerless needs an existing version tag before it can plan subsequent releases.
The actual first release supplies that boundary. The historical commit
`7023b93b3abc6860960bad6fa6acdb3807236a84` introduced `version.txt`; it does not
contain the current library and must not be used as the published `v0.1.0` tag.
If a tag or registry version already exists, inspect it rather than recreating
it. Never move an existing release tag or overwrite package assets.

## Normal release flow

After bootstrap, default-branch changes let Runnerless propose a release PR
updating `version.txt` and `CHANGELOG.md`. A merged release PR is finalized into
its immutable tag and published GitHub Release.

`.github/workflows/publish-release.yml` checks out that exact tag, stages public
manifest/dependency versions, checks all five public packages on Rust 1.85.0,
and tests their features on stable Rust.
The non-publishable kernel codec's `kernel` feature supplies a kernel panic
handler and belongs to its kernel build, rather than the userspace public-package
release tests. Package verification then captures Cargo
upload bodies locally, then attaches them and `SHA256SUMS` to the release.
Runnerless verifies the matching stable release and assets before uploading
`git-zero-core`, `git-zero-client`, `git-zero-server`, `git-zero` and
`git-zero-rustix`, in that dependency order. Registry credentials stay in the
configured Runnerless publishing destination.

## Local validation

```sh
python3 -m unittest discover -s tests -p 'test_release*.py'
python3 scripts/check-public-packages.py --release
cargo test --all-features --locked \
  -p git-zero-core -p git-zero-client -p git-zero-server \
  -p git-zero -p git-zero-rustix
python3 scripts/package-registry-uploads.py /tmp/git-zero-registry-uploads
```

The capture command builds and verifies each package against a loopback registry,
then records the exact Cargo upload bodies and checksums in dependency order.
That registry never forwards uploads to crates.io. Inspect the archived README,
manifest, source, and license files before publishing. README links to guides use
repository URLs because those guides are not included in the crate archives.

After publication, verify each released registry entry, dependency requirements,
and downloadable archive, then verify the corresponding docs.rs build. A green
package-preparation workflow proves only that assets were attached; a successful
Runnerless upload and the registry entries prove publication.

To stage another version, use a clean temporary checkout. Run
`python3 scripts/prepare-release.py vMAJOR.MINOR.PATCH` only when that version
matches its `version.txt`; the command edits manifests and `Cargo.lock`.
