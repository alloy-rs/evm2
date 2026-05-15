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
    diff -u "$upstream/$rel" "$port/$rel" > "$diff_file" || true
done
