#!/usr/bin/env bash
# Run a folder (or single file) of EEST JSON fixtures through the nextest harness.
#
# Usage:
#   scripts/eest.sh <path> [extra cargo-nextest args...]
#
# <path> may be absolute or relative to the current directory. Every JSON
# fixture found under it runs as one auto-detecting suite (state vs blockchain
# kind is detected per file), so no test-name filter is needed. Extra arguments
# are forwarded to `cargo nextest run`, for example:
#   scripts/eest.sh ./test-fixtures/devnet/state_tests -E 'test(precompile)'
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: scripts/eest.sh <path> [extra cargo-nextest args...]" >&2
    exit 2
fi

path="$1"
shift

if [[ ! -e "$path" ]]; then
    echo "path does not exist: $path" >&2
    exit 1
fi

# Resolve to an absolute path so the harness does not reinterpret it relative to
# the workspace root.
abs_path="$(cd "$(dirname "$path")" && pwd)/$(basename "$path")"

EVM2_ADDITIONAL_TESTS="$abs_path" exec cargo nextest run \
    -p evm2-eest --test eest --ignore-default-filter "$@"
