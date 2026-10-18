#!/usr/bin/env bash
# Offline documentation packaging only. Never launches Git/Worker correctness runners.
set -euo pipefail
repo=/tmp/git-zero-hosting-fit-response
pub_rel=docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r21-fit-response-candidate
pub="$repo/$pub_rel"
raw=/dev/shm/git-zero-fit-response-correctness-20261018
records=/tmp/git-zero-r21-copy-records.jsonl
raw_inventory=/tmp/git-zero-fit-response-raw-evidence-inventory-final.json
test "$(git -C "$repo" rev-parse HEAD)" = 95fdf58f6bd7298736cfe124725e4f7e6c9aceb9
test -z "$(git -C "$repo" status --porcelain)"
test ! -e "$pub"
test ! -e "$records"
mkdir -p "$pub"
: > "$records"

copy_original() {
    local src=$1 relative=$2 kind=$3 expected=${4:-} encoding=gzip
    local size sha out stored_size stored_sha utf8=true matches offsets='[]' pack=false
    test -f "$src"
    case "$relative" in /*|../*|*/../*) return 1;; esac
    out="$pub/$relative"
    if test -e "$out"; then return 0; fi
    size=$(stat -c %s "$src")
    test "$size" -le 8388608
    sha=$(sha256sum "$src"); sha=${sha%% *}
    if test -n "$expected"; then test "$sha" = "$expected"; fi
    local magic
    magic=$(od -An -tx1 -N16 "$src" | tr -d ' \n')
    case "$magic" in 7f454c46*|53514c69746520666f726d6174203300*) return 1;; esac
    if ! iconv -f UTF-8 -t UTF-8 "$src" >/dev/null 2>&1; then utf8=false; fi
    if matches=$(LC_ALL=C rg -a --byte-offset --no-filename -o --replace PACKV2 'PACK\x00\x00\x00\x02' "$src"); then
        test "$kind" = wire
        test "$size" -le 65536
        case "$src" in *.body|*.request) ;; *) return 1;; esac
        pack=true
        offsets=$(printf '%s\n' "$matches" | cut -d: -f1 | jq -Rsc 'split("\n")|map(select(length>0)|tonumber)')
    else
        test "$?" -eq 1
        test "$utf8" = true
    fi
    if test "$kind" = wire; then test "$size" -le 65536; fi
    mkdir -p "$(dirname "$out")"
    case "$relative" in *.gz) gzip -n -c "$src" > "$out";; *) encoding=identity; cp --preserve=timestamps "$src" "$out";; esac
    if test "$encoding" = gzip; then
        local restored_sha
        restored_sha=$(gzip -cd "$out" | sha256sum); restored_sha=${restored_sha%% *}
        test "$restored_sha" = "$sha"
        test "$(gzip -cd "$out" | wc -c)" -eq "$size"
    else
        cmp "$src" "$out"
    fi
    stored_sha=$(sha256sum "$out"); stored_sha=${stored_sha%% *}
    stored_size=$(stat -c %s "$out")
    jq -nc --arg source "$src" --arg path "$relative" --arg encoding "$encoding" --arg kind "$kind" --arg sha "$sha" --arg stored_sha "$stored_sha" --argjson bytes "$size" --argjson stored_bytes "$stored_size" --argjson utf8 "$utf8" --argjson pack "$pack" --argjson offsets "$offsets" '{source_path:$source,stored_path:$path,encoding:$encoding,kind:$kind,source_bytes:$bytes,source_sha256:$sha,stored_bytes:$stored_bytes,stored_sha256:$stored_sha,UTF8:$utf8,PACK_v2_signature:$pack,PACK_v2_offsets:$offsets}' >> "$records"
}

# Immutable review directories: original reports, input seals, failed drafts and audit sources.
for pair in 'preliminary /tmp/git-zero-fit-response-independent-review' 'runner-v2 /tmp/git-zero-fit-response-independent-review-supplement-01' 'final /tmp/git-zero-fit-response-independent-final-review'; do
    read -r label folder <<< "$pair"
    while IFS= read -r -d '' src; do
        name=${src##*/}; suffix=.gz
        case "$name" in *.md|*.py|*.diff|*SHA256SUMS) suffix=;; esac
        copy_original "$src" "reviews/$label/$name$suffix" review
    done < <(rg --files --hidden --no-ignore -0 "$folder" | sort -z)
done
for src in /tmp/git-zero-fit-response-independent-design-review.md /tmp/git-zero-fit-response-independent-design-review.json /tmp/git-zero-fit-response-independent-design-review.py /tmp/git-zero-fit-response-independent-design-review.calculation.json; do
    name=${src##*/}; suffix=.gz
    case "$name" in *.md|*.py) suffix=;; esac
    copy_original "$src" "reviews/design/$name$suffix" review
done

for stem in source-handoff controls-supplement-01 runner-v2-handoff completion-handoff; do
    copy_original "/tmp/git-zero-fit-response-$stem.md" "handoffs/$stem.md" handoff
    copy_original "/tmp/git-zero-fit-response-$stem.json" "handoffs/$stem.json.gz" handoff
done
for name in final-evidence-index completed-v2-gates-final fault-coverage-final owned-cleanup-final runtime-inventory-final source-ownership-v0 pair-comparison-v0 pair-comparison-v0-first-schema pair-comparison-v0-second-schema raw-evidence-inventory raw-evidence-inventory-supplement-01 raw-evidence-inventory-final compact-mirror-inventory compact-mirror-inventory-final clippy-final-preflight; do
    copy_original "/tmp/git-zero-fit-response-$name.json" "evidence/$name.json.gz" metadata
done
for version in v0 v1 v2; do
    copy_original "/tmp/git-zero-fit-response-build-$version/manifest.json" "source-maps/$version-manifest.json.gz" metadata
    copy_original "/tmp/git-zero-fit-response-build-$version/runner.rs" "source-maps/$version-runner.rs.gz" source
done
copy_original /tmp/git-zero-fit-response-final-source/manifest.json source-maps/final-manifest.json.gz metadata
copy_original /tmp/git-zero-fit-response-final-source/runner.rs source-maps/final-runner.rs.gz source
copy_original /tmp/git-zero-fit-response-final-source/native.rs source-maps/native.rs.gz source
copy_original /tmp/git-zero-fit-response-final-source/cli.rs source-maps/cli.rs.gz source
copy_original /tmp/git-zero-fit-response-runner-final-lint-only.patch source-maps/tested-v2-to-final.patch source
for src in /tmp/git-zero-fit-response-*.log; do
    copy_original "$src" "logs/${src##*/}.gz" log
done
for src in /tmp/git-zero-fit-response-*.resource-outer-*.json; do
    copy_original "$src" "resources/${src##*/}.gz" metadata
done

# Curated actual raw originals: top-level contracts, maps, receipts and small wire samples.
# Generated repositories, R2/SQLite state, archives, standalone packs and ELF payloads never enter.
jq -r '.files[]|(.relative_path|split("/")) as $p|select(($p|length)==1 or ($p|length)==2)|select((.relative_path|test("\\.(json|log|toml)$")) or (.bytes<=65536 and (.relative_path|test("\\.(request|body)$")) and ((.relative_path|test("\\.request$")) or (.relative_path|test("(final|observer|startup).*\\.body$")))))|[.path,.relative_path,(.bytes|tostring),.sha256]|@tsv' "$raw_inventory" > /tmp/git-zero-r21-raw-selection.tsv
# Every small PACK-bearing body is eligible, even if its label is not a final observer.
while IFS=$'\t' read -r src relative size sha; do
    if LC_ALL=C rg -a -q 'PACK\x00\x00\x00\x02' "$src"; then
        printf '%s\t%s\t%s\t%s\n' "$src" "$relative" "$size" "$sha" >> /tmp/git-zero-r21-raw-selection.tsv
    fi
done < <(jq -r '.files[]|select(.bytes<=65536)|select(.relative_path|test("^[^/]+/[^/]+\\.body$"))|[.path,.relative_path,(.bytes|tostring),.sha256]|@tsv' "$raw_inventory")
# Retain complete transport evidence for every non-200 attempt and selected final/startup observers.
while IFS= read -r -d '' status_file; do
    status=$(cat "$status_file")
    base=${status_file%.status}
    case "$base" in *final*|*observer*|*startup*) selected=true;; *) selected=false;; esac
    if test "$status" != 200 || test "$selected" = true; then
        for extension in body headers status stderr exit; do
            src="$base.$extension"
            if test -f "$src" && test "$(stat -c %s "$src")" -le 65536; then
                relative=${src#"$raw/"}; sha=$(sha256sum "$src"); sha=${sha%% *}
                printf '%s\t%s\t%s\t%s\n' "$src" "$relative" "$(stat -c %s "$src")" "$sha" >> /tmp/git-zero-r21-raw-selection.tsv
            fi
        done
    fi
done < <(rg --files --hidden --no-ignore -0 "$raw" -g '*.status' | sort -z)
sort -u /tmp/git-zero-r21-raw-selection.tsv > /tmp/git-zero-r21-raw-selection-sorted.tsv
while IFS=$'\t' read -r src relative size sha; do
    kind=metadata
    case "$relative" in *.body|*.request|*.headers|*.status|*.stderr|*.exit) kind=wire;; esac
    copy_original "$src" "controls/$relative.gz" "$kind" "$sha"
done < /tmp/git-zero-r21-raw-selection-sorted.tsv

copy_original /tmp/git-zero-fit-response-build-r21-ledger.sh author/build-ledger.sh source
jq -s --arg utc "$(date -u +%FT%TZ)" '{utc:$utc,kind:"exact identity/gzip copies of selected originals; originals not reserialized",gzip_header:"gzip -n; original bytes recovered by gzip -cd",files:(sort_by(.stored_path))}' "$records" > "$pub/COPY-MAP.json"
jq -s '{kind:"positive classification of every published decompressed original payload",PACK_v2_signature_hex:"5041434b00000002",PACK_bearing_wire_limit_bytes:65536,all_wire_original_limit_bytes:65536,text_metadata_source_limit_bytes:8388608,PACK_v2_bearing_files:(map(select(.PACK_v2_signature))),PACK_full_original_file_bytes:(map(select(.PACK_v2_signature)|.source_bytes)|add//0),PACK_file_count:(map(select(.PACK_v2_signature))|length),non_UTF8_files:(map(select(.UTF8==false)|{stored_path,source_bytes,source_sha256,PACK_v2_signature})),all_non_UTF8_files_are_declared_PACK_samples:all(.UTF8 or .PACK_v2_signature),ELF_payload_files:0,SQLite_payload_files:0,generated_repository_R2_SQLite_state_files:0,large_wire_payload_files:0,whole_wanted_archives_or_standalone_seed_pack_files:0,signature_offset_scope:"Offsets locate literal PACK-v2 signatures in actual full synthetic request/response streams. No decoded-object or parsed pack-span-size inference.",no_redundant_archive_capsule:true}' "$records" > "$pub/protocol-evidence-inventory.json"
jq '{copied_original_files:(.files|length),original_bytes:(.files|map(.source_bytes)|add),stored_bytes:(.files|map(.stored_bytes)|add)}' "$pub/COPY-MAP.json"
jq '{PACK_file_count,PACK_full_original_file_bytes,non_UTF8_files:(.non_UTF8_files|length)}' "$pub/protocol-evidence-inventory.json"
