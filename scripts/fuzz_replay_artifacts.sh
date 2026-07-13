#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
FUZZ_MANIFEST="$ROOT/fuzz/Cargo.toml"

CARGO_BIN="${CARGO:-cargo}"
RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-nightly}"
CARGO_FUZZ_RUN_ARGS="${CARGO_FUZZ_RUN_ARGS:---features jit}"
LIBFUZZER_ARGS="${LIBFUZZER_ARGS:--runs=1 -timeout=300 -ignore_ooms=1 -rss_limit_mb=8192}"
ARTIFACT_GLOB="${FUZZ_REPLAY_ARTIFACT_GLOB:-crash-*}"
LOG_ROOT="${FUZZ_REPLAY_LOG_ROOT:-$ROOT/target/fuzz-replay-logs/$(date -u +%Y%m%dT%H%M%SZ)}"
KEEP_GOING=0
DRY_RUN=0
TARGET_FILTERS=()
CLI_LIBFUZZER_ARGS=()

if [[ -z "${LLVM_SYS_221_PREFIX:-}" && -x /usr/lib/llvm-22/bin/llvm-config ]]; then
    export LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/fuzz-replay-target}"

usage() {
    cat <<'EOF'
Usage:
  scripts/fuzz_replay_artifacts.sh [options] [target-glob ...] [-- libFuzzer-arg ...]

Replay saved cargo-fuzz artifacts as fixed inputs. This does not start open-ended fuzzing:
each selected target is invoked with explicit artifact files and libFuzzer defaults to
-runs=1 -timeout=300 -ignore_ooms=1 -rss_limit_mb=8192.

Options:
  --artifact-glob GLOB  Artifact filename glob per target. Default: crash-*.
  --log-root DIR        Directory for summary and per-target logs.
                        Default: target/fuzz-replay-logs/<utc timestamp>.
  --keep-going          Continue replaying remaining targets after a failure.
  --dry-run             Print commands without running them.
  -h, --help            Show this help.

Environment:
  CARGO_FUZZ_RUN_ARGS       Extra cargo-fuzz run args. Default: --features jit
  LIBFUZZER_ARGS            Default libFuzzer args appended after '--'.
  FUZZ_REPLAY_ARTIFACT_GLOB Same as --artifact-glob.
  FUZZ_REPLAY_LOG_ROOT      Same as --log-root.

Examples:
  scripts/fuzz_replay_artifacts.sh
  scripts/fuzz_replay_artifacts.sh structured_compare_amsterdam
  scripts/fuzz_replay_artifacts.sh --artifact-glob 'timeout-*' lifecycle_compare_*
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

split_words() {
    local -n out="$1"
    local value="$2"
    if [[ -n "$value" ]]; then
        # Intentional shell-style word splitting for simple env-provided args.
        read -r -a out <<<"$value"
    else
        out=()
    fi
}

discover_targets() {
    awk '
        /^\[\[bin\]\]/ { in_bin = 1; next }
        /^\[/ { in_bin = 0 }
        in_bin && $1 == "name" {
            line = $0
            sub(/^[^"]*"/, "", line)
            sub(/".*$/, "", line)
            print line
        }
    ' "$FUZZ_MANIFEST"
}

select_targets() {
    local filters=("$@")
    local target pattern

    while IFS= read -r target; do
        if ((${#filters[@]} == 0)); then
            printf '%s\n' "$target"
            continue
        fi

        for pattern in "${filters[@]}"; do
            if [[ "$target" == $pattern ]]; then
                printf '%s\n' "$target"
                break
            fi
        done
    done < <(discover_targets)
}

print_command() {
    printf 'command:'
    printf ' %q' "$@"
    printf '\n'
}

replay_target() {
    local target="$1"
    local log="$2"
    local -n out_executed="$3"
    local cargo_args=()
    local env_libfuzzer_args=()
    local libfuzzer_args=()

    split_words cargo_args "$CARGO_FUZZ_RUN_ARGS"
    split_words env_libfuzzer_args "$LIBFUZZER_ARGS"
    libfuzzer_args=("${env_libfuzzer_args[@]}" "${CLI_LIBFUZZER_ARGS[@]}")

    shopt -s nullglob
    local files=("$ROOT/fuzz/artifacts/$target"/$ARTIFACT_GLOB)
    shopt -u nullglob

    if ((${#files[@]} == 0)); then
        out_executed=0
        return 2
    fi

    local command=(
        "$CARGO_BIN" "+$RUSTUP_TOOLCHAIN" fuzz run
        "${cargo_args[@]}"
        "$target"
        "${files[@]}"
        --
        "${libfuzzer_args[@]}"
    )

    if ((DRY_RUN)); then
        print_command "${command[@]}" | tee "$log"
        out_executed=${#files[@]}
        return 0
    fi

    if "${command[@]}" >"$log" 2>&1; then
        out_executed="$(grep -c '^Executed ' "$log" || true)"
        return 0
    fi

    out_executed="$(grep -c '^Executed ' "$log" || true)"
    return 1
}

while (($#)); do
    case "$1" in
        --artifact-glob)
            [[ $# -ge 2 ]] || die "--artifact-glob requires a value"
            ARTIFACT_GLOB="$2"
            shift 2
            ;;
        --log-root)
            [[ $# -ge 2 ]] || die "--log-root requires a value"
            LOG_ROOT="$2"
            shift 2
            ;;
        --keep-going)
            KEEP_GOING=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        --)
            shift
            CLI_LIBFUZZER_ARGS=("$@")
            break
            ;;
        --*)
            die "unknown option '$1'"
            ;;
        *)
            TARGET_FILTERS+=("$1")
            shift
            ;;
    esac
done

mapfile -t SELECTED_TARGETS < <(select_targets "${TARGET_FILTERS[@]}")
if ((${#SELECTED_TARGETS[@]} == 0)); then
    die "no matching fuzzer targets"
fi

mkdir -p "$LOG_ROOT"
summary="$LOG_ROOT/summary"
: >"$summary"

total_targets=0
total_artifacts=0
failed_targets=0

for target in "${SELECTED_TARGETS[@]}"; do
    log="$LOG_ROOT/$target.log"
    printf '== %s ==\n' "$target" | tee -a "$summary"
    executed=0
    status=0
    replay_target "$target" "$log" executed || status=$?
    case "$status" in
        0)
            total_targets=$((total_targets + 1))
            total_artifacts=$((total_artifacts + executed))
            printf 'PASS %s executed=%s log=%s\n' "$target" "$executed" "$log" | tee -a "$summary"
            ;;
        2)
            printf 'SKIP %s matched=0 glob=%s\n' "$target" "$ARTIFACT_GLOB" | tee -a "$summary"
            ;;
        *)
            total_targets=$((total_targets + 1))
            total_artifacts=$((total_artifacts + executed))
            failed_targets=$((failed_targets + 1))
            printf 'FAIL %s executed=%s log=%s\n' "$target" "$executed" "$log" | tee -a "$summary"
            tail -120 "$log" || true
            if ((KEEP_GOING == 0)); then
                break
            fi
            ;;
    esac
done
{
    printf 'SUMMARY targets=%s artifacts=%s failures=%s log_root=%s\n' \
        "$total_targets" "$total_artifacts" "$failed_targets" "$LOG_ROOT"
} | tee -a "$summary"

if ((total_targets == 0)); then
    printf 'error: no artifacts matched %s for selected targets\n' "$ARTIFACT_GLOB" >&2
    exit 1
fi

if ((failed_targets != 0)); then
    exit 1
fi
