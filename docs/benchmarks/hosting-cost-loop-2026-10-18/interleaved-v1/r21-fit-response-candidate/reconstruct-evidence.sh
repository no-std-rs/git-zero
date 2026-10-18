#!/usr/bin/env bash
# Reconstruct only selected published compact metadata/protocol bytes into a NEW directory.
set -euo pipefail
test "$#" -eq 1
destination=$1
test ! -e "$destination"
mkdir -p -- "$destination"
destination=$(cd -- "$destination" && pwd -P)
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
bash verify-publication.sh
tar -xzf selected-evidence.tar.gz -C "$destination" --no-same-owner --no-same-permissions
while IFS=$'\t' read -r stored encoding; do
    if test "$encoding" = tar-member; then continue; fi
    case "$stored" in /*|../*|*/../*) exit 1;; esac
    relative=$stored
    if test "$encoding" = gzip; then relative=${relative%.gz}; fi
    output="$destination/$relative"
    test ! -e "$output"
    mkdir -p -- "$(dirname -- "$output")"
    if test "$encoding" = gzip; then gzip -cd -- "$stored" > "$output"; else cp -- "$stored" "$output"; fi
done < <(jq -r '.files[]|[.stored_path,.encoding]|@tsv' COPY-MAP.json)
cp -- COPY-MAP.json "$destination/COPY-MAP.json"
printf 'Only published compact originals reconstructed in %s\n' "$destination"
