#!/usr/bin/env bash
# Offline compact-file validation only; no service, build, Git operation or raw-state restore.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
sha256sum --check SHA256SUMS >/dev/null
jq -e '(.files|length)==(.files|unique_by([.stored_path,.archive_member//""])|length)' COPY-MAP.json >/dev/null
verification_dir=$(mktemp -d "${TMPDIR:-/tmp}/r21-publication-verify.XXXXXXXX")
finish() {
    result=$?
    if test "$result" -eq 0; then rm -rf -- "$verification_dir"; else printf 'Failed verification retained at %s\n' "$verification_dir" >&2; fi
    exit "$result"
}
trap finish EXIT
tar -tzf selected-evidence.tar.gz > "$verification_dir/members"
tar -tvzf selected-evidence.tar.gz > "$verification_dir/types"
if LC_ALL=C rg -n '(^/|(^|/)\.\.(/|$))' "$verification_dir/members"; then exit 1; fi
if LC_ALL=C rg -n '^[^-d]' "$verification_dir/types"; then exit 1; fi
jq -r '.files[]|select(.encoding=="tar-member")|.archive_member' COPY-MAP.json > "$verification_dir/expected"
printf 'SHA256SUMS\n' >> "$verification_dir/expected"
LC_ALL=C sort "$verification_dir/expected" > "$verification_dir/expected-sorted"
LC_ALL=C rg -v '/$' "$verification_dir/members" | LC_ALL=C sort > "$verification_dir/actual-sorted"
cmp "$verification_dir/expected-sorted" "$verification_dir/actual-sorted"
mkdir "$verification_dir/content"
tar -xzf selected-evidence.tar.gz -C "$verification_dir/content" --no-same-owner --no-same-permissions
jq -r '.files[]|select(.encoding=="tar-member")|.source_sha256+"  "+.archive_member' COPY-MAP.json > "$verification_dir/expected-SHA256SUMS"
cmp "$verification_dir/expected-SHA256SUMS" "$verification_dir/content/SHA256SUMS"
(cd "$verification_dir/content" && sha256sum --check SHA256SUMS >/dev/null)
while IFS=$'\t' read -r stored encoding expected_sha expected_bytes member; do
    case "$stored" in /*|../*|*/../*) exit 1;; esac
    if test "$encoding" = gzip; then
        actual_sha=$(gzip -cd -- "$stored" | sha256sum); actual_sha=${actual_sha%% *}
        actual_bytes=$(gzip -cd -- "$stored" | wc -c)
    elif test "$encoding" = identity; then
        actual_sha=$(sha256sum -- "$stored"); actual_sha=${actual_sha%% *}
        actual_bytes=$(stat -c %s -- "$stored")
    elif test "$encoding" = tar-member; then
        actual_sha=$(sha256sum -- "$verification_dir/content/$member"); actual_sha=${actual_sha%% *}
        actual_bytes=$(stat -c %s -- "$verification_dir/content/$member")
    else
        exit 1
    fi
    test "$actual_sha" = "$expected_sha"
    test "$actual_bytes" -eq "$expected_bytes"
done < <(jq -r '.files[]|[.stored_path,.encoding,.source_sha256,(.source_bytes|tostring),(.archive_member//"-")]|@tsv' COPY-MAP.json)
printf 'Stored checksums and every exact decompressed original verified.\n'
