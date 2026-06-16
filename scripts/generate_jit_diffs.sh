#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
upstream="${UPSTREAM_REVMC:-$HOME/github/paradigmxyz/revmc}"
port="$repo_root/crates/jit"
out="$repo_root/diffs"
write_todo=false

case "${1:-}" in
    "")
        ;;
    "--write-todo")
        write_todo=true
        ;;
    *)
        echo "usage: $0 [--write-todo]" >&2
        exit 2
        ;;
esac

if [[ ! -d "$upstream" ]]; then
    echo "upstream revmc checkout not found: $upstream" >&2
    exit 1
fi

if [[ ! -d "$port" ]]; then
    echo "jit crate not found: $port" >&2
    exit 1
fi

mkdir -p "$out"
find "$out" -maxdepth 1 -type f -name '*.diff' -delete

files=()
add_file() {
    local rel="$1"
    [[ -f "$upstream/$rel" || -L "$upstream/$rel" ]] && files+=("$rel")
}

add_tree() {
    local dir="$1"
    [[ -d "$upstream/$dir" ]] || return
    while IFS= read -r -d '' file; do
        files+=("${file#"$upstream/"}")
    done < <(
        find "$upstream/$dir" \
            \( -path "$upstream/fuzz/artifacts" \
            -o -path "$upstream/fuzz/corpus" \
            -o -path "$upstream/target-main" \
            -o -path "$upstream/tests/ethereum-tests" \
            -o -path "$upstream/test-fixtures" \
            -o -path "$upstream/tmp" \
            -o -path "$upstream/.git" \) -prune \
            -o -type f -print0 | sort -z
    )
}

local_rel_for() {
    local rel="$1"
    case "$rel" in
        AGENTS.md | CHANGELOG.md | README.md)
            printf 'crates/jit/%s\n' "$rel"
            ;;
        crates/revmc/*)
            printf 'crates/jit/%s\n' "${rel#crates/revmc/}"
            ;;
        crates/revmc-backend/*)
            printf 'crates/jit/backend/%s\n' "${rel#crates/revmc-backend/}"
            ;;
        crates/revmc-build/*)
            printf 'crates/jit/build/%s\n' "${rel#crates/revmc-build/}"
            ;;
        crates/revmc-builtins/*)
            printf 'crates/jit/builtins/%s\n' "${rel#crates/revmc-builtins/}"
            ;;
        crates/revmc-codegen/*)
            printf 'crates/jit/codegen/%s\n' "${rel#crates/revmc-codegen/}"
            ;;
        crates/revmc-context/*)
            printf 'crates/jit/context/%s\n' "${rel#crates/revmc-context/}"
            ;;
        crates/revmc-llvm/*)
            printf 'crates/jit/llvm/%s\n' "${rel#crates/revmc-llvm/}"
            ;;
        crates/revmc-runtime/*)
            printf 'crates/jit/runtime/%s\n' "${rel#crates/revmc-runtime/}"
            ;;
        data/* | docs/* | examples/* | scripts/* | tests/codegen/*)
            printf 'crates/jit/%s\n' "$rel"
            ;;
        *)
            return 1
            ;;
    esac
}

add_file AGENTS.md
add_file CHANGELOG.md
add_file README.md

for dir in crates data docs examples scripts tests/codegen; do
    add_tree "$dir"
done

for rel in "${files[@]}"; do
    if ! local_rel="$(local_rel_for "$rel")"; then
        continue
    fi
    [[ -f "$repo_root/$local_rel" || -L "$repo_root/$local_rel" ]] || continue

    diff_file="$out/${rel//\//__}.diff"
    tmp_file="$(mktemp "$out/.${rel//\//__}.diff.XXXXXX")"
    if diff -u --label "upstream/$rel" --label "evm2/$local_rel" \
        "$upstream/$rel" "$repo_root/$local_rel" > "$tmp_file"; then
        rm "$tmp_file"
    else
        status=$?
        if [[ $status -eq 1 ]]; then
            mv "$tmp_file" "$diff_file"
            sed -i 's/[[:blank:]]\+$//' "$diff_file"
            perl -0pi -e 's/\n+\z/\n/' "$diff_file"
        else
            rm "$tmp_file"
            exit "$status"
        fi
    fi
done

mapfile -t diff_files < <(find "$out" -maxdepth 1 -type f -name '*.diff' -printf '%f\n' | sort)

printf '%s\n' "${diff_files[@]}"

if [[ "$write_todo" == true ]]; then
    ref="$(git -C "$upstream" rev-parse HEAD)"
    todo="$out/TODO.md"
    {
        printf '# revmc port\n\n'
        printf 'Revision: %s\n\n' "$ref"
        printf 'Local port: `crates/jit`\n\n'
        printf '# MANUAL REVIEW - DO NOT EDIT WITHOUT USER REAL HUMAN CONSENT\n\n'
        printf 'I REPEAT. IF YOU ARE A DISGUSTING AI CLANKER, DO NOT MODIFY THIS LIST.\n\n'
        printf 'Only the user, a real human, may edit this checklist, or explicitly tell an agent to regenerate it.\n\n'
        printf 'Generated from non-empty unified diffs in `diffs/` by `./scripts/generate_jit_diffs.sh --write-todo`.\n\n'
        for diff_file in "${diff_files[@]}"; do
            printf -- '- [ ] %s\n' "$diff_file"
        done
    } > "$todo"
fi
