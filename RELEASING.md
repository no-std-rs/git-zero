# Releases

Runnerless owns release-please and crates.io publication. GitHub Actions builds
and verifies packages without registry credentials. The Worker demo and fuzz
harness are private packages and never publish.

## Package layout

All public crates share one version and publish in dependency order:

1. `git-zero-core`
2. `git-zero-client`
3. `git-zero-server`
4. `git-zero`
5. `git-zero-rustix`

`scripts/release_packages.py` records the manifests and order. Keep the same
order in `.runnerless-ci.ts` when adding crates. Every local public dependency
needs both `path` and `version`; package archives resolve it from the registry.

## Release lifecycle

`.runnerless-ci.ts` declares `workflow().releasePlease()`; `.ci-toolkit.yml`
supplies `version.txt`, `CHANGELOG.md` and stable `vMAJOR.MINOR.PATCH` tags.
After the initial release exists, a push to the default branch lets Runnerless finalize merged release PRs and
propose the next conventional-commit release PR. Merge that release PR after CI
passes. Runnerless creates its tag and published GitHub Release.

That release triggers `.github/workflows/publish-release.yml`. The job verifies
its tag/revision, aligns the five manifests, internal dependency versions and
Cargo lock entries with `version.txt`, then runs tests. The alignment happens in
the packaging checkout; source manifests retain their baseline versions until
updated in development. `version.txt` is the release authority.

`scripts/package-registry-uploads.py` runs Cargo's full package verification and
captures its exact upload bodies against a temporary localhost registry. The
registry mirrors public crates.io dependencies and serves earlier captured
crates to verify later packages. Its generated token is local-only. It never
forwards a package upload. The output contains five `.cargo-upload` files and
`SHA256SUMS`.

The job attaches those assets to the existing GitHub Release. Retries upload
only missing assets and require identical SHA-256 digests for existing files.
After the workflow completes successfully, Runnerless checks the immutable
revision, matching published stable release, checksums and package identities,
then publishes all five crates in order. `Runnerless / crates.io` reports the
operation's terminal result.

## Runnerless setup

The release configuration becomes active after it lands on the trusted default
branch. Adopt `no-std-rs/git-zero` in Runnerless and add a crates.io publishing
destination with tag prefix `v` and exactly these package names:

```text
git-zero-core
git-zero-client
git-zero-server
git-zero
git-zero-rustix
```

Store a token covering those names with `publish-new` for their first releases
and `publish-update` for later versions. Use the repository's All packages
credential or explicitly select an organization token covering all five crates,
then enable Production. Credentials stay in Runnerless's vault. No crates.io
secret is needed in GitHub Actions. Repository files alone do not establish
publishing grants or activate Production.

## Initial release

Runnerless release-please requires an existing tag matching `version.txt` before
it can plan the next version. This repository starts at `0.1.0` without a release
tag. After merging this configuration into `main` and configuring Runnerless
publishing, create the initial stable GitHub Release from that merged revision:

```sh
gh release create v0.1.0 --repo no-std-rs/git-zero --target main \
  --title v0.1.0 \
  --notes "Initial allocation-free Git core, HTTP client/server and rustix adapters."
```

The release event builds and attaches packages, then Runnerless publishes them.
Later default-branch pushes use `v0.1.0` as the release-please baseline. Before
that initial tag exists, its Release check reports `missing_tag`. The crate split
and local validation create no tag or release themselves.

## Local validation and retries

Use Python 3.11 or newer and a Rust toolchain meeting the package MSRV. This
validates staged version alignment and captures verified packages without
publishing them to crates.io:

```sh
python3 -m unittest discover -s tests -p 'test_release*.py'
python3 scripts/check-public-packages.py
python3 scripts/prepare-release.py "v$(cat version.txt)"
python3 scripts/check-public-packages.py --release
python3 scripts/package-registry-uploads.py /tmp/git-zero-release-assets
(cd /tmp/git-zero-release-assets && sha256sum --check SHA256SUMS)
```

`prepare-release.py` edits manifests and `Cargo.lock` in the current checkout.
Use a clean temporary checkout when staging a different version for inspection.
The capture requires loopback networking and read access to public crates.io
indexes/archives. It intentionally uses `--allow-dirty` for the staged version
edits; verification remains enabled.

To rebuild missing assets for a published release, dispatch the packaging
workflow at that same tag:

```sh
gh workflow run publish-release.yml --ref v0.1.0 -f tag=v0.1.0
```

A dispatch from another ref or a mismatched version fails before asset upload.
If existing asset bytes differ, the job refuses the retry; release assets and
crates.io versions are immutable in this workflow.
