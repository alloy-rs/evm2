#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
upstream="${UPSTREAM_REVM_INSPECTORS:-$HOME/github/paradigmxyz/revm-inspectors}"
port="$repo_root/crates/inspectors"
out="$repo_root/diffs"

if [[ ! -d "$upstream" ]]; then
    echo "upstream revm-inspectors checkout not found: $upstream" >&2
    exit 1
fi

if [[ ! -d "$port" ]]; then
    echo "inspectors crate not found: $port" >&2
    exit 1
fi

mkdir -p "$out"
find "$out" -maxdepth 1 -type f -name '*.diff' -delete

files=()
for rel in Cargo.toml README.md; do
    [[ -f "$upstream/$rel" ]] && files+=("$rel")
done

for dir in src testdata tests; do
    [[ -d "$upstream/$dir" ]] || continue
    while IFS= read -r -d '' file; do
        files+=("${file#"$upstream/"}")
    done < <(find "$upstream/$dir" -type f -print0 | sort -z)
done

for rel in "${files[@]}"; do
    [[ -f "$port/$rel" ]] || continue
    diff_file="$out/${rel//\//__}.diff"
    tmp_file="$(mktemp "$out/.${rel//\//__}.diff.XXXXXX")"
    if diff -u --label "upstream/$rel" --label "evm2/$rel" "$upstream/$rel" "$port/$rel" > "$tmp_file"; then
        rm "$tmp_file"
    else
        status=$?
        if [[ $status -eq 1 ]]; then
            mv "$tmp_file" "$diff_file"
        else
            rm "$tmp_file"
            exit "$status"
        fi
    fi
done
