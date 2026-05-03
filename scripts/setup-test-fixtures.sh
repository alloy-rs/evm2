#!/usr/bin/env bash
# Download Ethereum state test fixtures into the repo-relative layout used by
# evm2-statetest, revm, and revmc.
#
# Layout:
#   test-fixtures/
#   ├── main/
#   │   ├── stable/state_tests/...
#   │   └── develop/state_tests/...
#   └── legacytests/
#       ├── Cancun/GeneralStateTests/...
#       └── Constantinople/GeneralStateTests/...

set -euo pipefail

MAIN_VERSION="${MAIN_VERSION:-v5.3.0}"
BASE_URL="https://github.com/ethereum/execution-spec-tests/releases/download"
LEGACY_REPO_URL="https://github.com/ethereum/legacytests.git"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURES_DIR="${EVM2_TEST_FIXTURES:-$REPO_ROOT/test-fixtures}"

MAIN_STABLE_DIR="$FIXTURES_DIR/main/stable"
MAIN_DEVELOP_DIR="$FIXTURES_DIR/main/develop"
LEGACY_DIR="$FIXTURES_DIR/legacytests"

download_and_extract() {
    local url="$1"
    local dest="$2"

    if [[ -d "$dest/state_tests" ]]; then
        echo "  Already exists: $dest/state_tests"
        return
    fi

    echo "  Downloading: $url"
    mkdir -p "$dest"
    curl -fsSL --retry 3 --retry-delay 2 --retry-all-errors "$url" |
        tar xz --strip-components=1 -C "$dest"
}

legacy_tests_exist() {
    [[ -d "$LEGACY_DIR/Cancun/GeneralStateTests" &&
        -d "$LEGACY_DIR/Constantinople/GeneralStateTests" ]]
}

echo "=== Downloading execution-spec-tests ($MAIN_VERSION) ==="
download_and_extract "$BASE_URL/$MAIN_VERSION/fixtures_stable.tar.gz" "$MAIN_STABLE_DIR"
download_and_extract "$BASE_URL/$MAIN_VERSION/fixtures_develop.tar.gz" "$MAIN_DEVELOP_DIR"

echo "=== Cloning ethereum/legacytests ==="
if legacy_tests_exist; then
    echo "  Already exists: $LEGACY_DIR"
elif [[ -e "$LEGACY_DIR" ]]; then
    echo "  Exists but does not contain the expected legacy state tests: $LEGACY_DIR" >&2
    exit 1
else
    git clone --depth 1 "$LEGACY_REPO_URL" "$LEGACY_DIR"
fi

echo "=== Done ==="
echo "Fixture directories:"
for dir in \
    "$MAIN_STABLE_DIR/state_tests" \
    "$MAIN_DEVELOP_DIR/state_tests" \
    "$LEGACY_DIR/Cancun/GeneralStateTests" \
    "$LEGACY_DIR/Constantinople/GeneralStateTests"
do
    if [[ -d "$dir" ]]; then
        count="$(find -L "$dir" -name '*.json' | wc -l | tr -d ' ')"
        echo "  $dir ($count JSON files)"
    fi
done
