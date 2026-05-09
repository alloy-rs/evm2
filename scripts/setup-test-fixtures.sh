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
LEGACY_VERSION="${LEGACY_VERSION:-v17.2}"
BASE_URL="https://github.com/ethereum/execution-spec-tests/releases/download"
LEGACY_URL="https://github.com/ethereum/tests/archive/refs/tags/$LEGACY_VERSION.tar.gz"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURES_DIR="${EVM2_TEST_FIXTURES:-$REPO_ROOT/test-fixtures}"

MAIN_STABLE_DIR="$FIXTURES_DIR/main/stable"
MAIN_DEVELOP_DIR="$FIXTURES_DIR/main/develop"
LEGACY_DIR="$FIXTURES_DIR/legacytests"

if [[ -n "${EVM2_STATETEST_STABLE:-}" && "${EVM2_STATETEST_STABLE:-}" != "0" ]]; then
    MAIN_DIR="$MAIN_STABLE_DIR"
    MAIN_TAR="fixtures_stable.tar.gz"
    MAIN_LABEL="main stable"
else
    MAIN_DIR="$MAIN_DEVELOP_DIR"
    MAIN_TAR="fixtures_develop.tar.gz"
    MAIN_LABEL="main develop"
fi

retry() {
    local attempts=5
    local delay=2
    local attempt=1

    until "$@"; do
        if [[ "$attempt" -ge "$attempts" ]]; then
            return 1
        fi
        echo "  Attempt $attempt failed. Retrying in ${delay}s..."
        sleep "$delay"
        attempt=$((attempt + 1))
        delay=$((delay * 2))
    done
}

curl_tar() {
    local url="$1"
    local dest="$2"

    curl -fL --progress-bar "$url" |
        tar xzf - --strip-components=1 -C "$dest"
}

download_and_extract() {
    local dest="$1"
    local tar_file="$2"
    local label="$3"
    local version="$4"

    if [[ -d "$dest/state_tests" ]]; then
        echo "  Already exists: $dest/state_tests"
        return
    fi

    local url="$BASE_URL/$version/$tar_file"

    echo "  Downloading and extracting: $url"
    mkdir -p "$dest" "$FIXTURES_DIR"
    retry curl_tar "$url" "$dest"
}

legacy_tests_exist() {
    [[ -d "$LEGACY_DIR/Cancun/GeneralStateTests" &&
        -d "$LEGACY_DIR/Constantinople/GeneralStateTests" ]]
}

download_legacy_tests() {
    if legacy_tests_exist; then
        echo "  Already exists: $LEGACY_DIR"
    elif [[ -e "$LEGACY_DIR" ]]; then
        echo "  Exists but does not contain the expected legacy state tests: $LEGACY_DIR" >&2
        return 1
    else
        echo "  Downloading and extracting: $LEGACY_URL"
        mkdir -p "$LEGACY_DIR" "$FIXTURES_DIR"
        retry curl_tar "$LEGACY_URL" "$LEGACY_DIR"
    fi
}

echo "=== Fetching state test fixtures ==="
download_and_extract "$MAIN_DIR" "$MAIN_TAR" "$MAIN_LABEL" "$MAIN_VERSION" &
main_pid=$!
download_legacy_tests &
legacy_pid=$!

status=0
for pid in "$main_pid" "$legacy_pid"; do
    if ! wait "$pid"; then
        status=1
    fi
done
if [[ "$status" -ne 0 ]]; then
    exit "$status"
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
