#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/fuzz_screen.sh"
FUZZ_MANIFEST="$ROOT/fuzz/Cargo.toml"
ENV_FILE="${FUZZ_SCREEN_ENV_FILE:-$ROOT/.fuzz_screeen.env}"
if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck source=/dev/null
    source "$ENV_FILE"
    set +a
fi
DEFAULT_CRASH_HOOK="$ROOT/scripts/slack_fuzz_crash_hook.sh"

SESSION_PREFIX="${FUZZ_SCREEN_PREFIX:-evm2-fuzz}"
LOG_ROOT="${FUZZ_LOG_ROOT:-$ROOT/target/fuzz-screen-logs}"
LOG_ROTATE_SIZE="${FUZZ_LOG_ROTATE_SIZE:-100M}"
CRASH_LOG="${FUZZ_CRASH_LOG:-$LOG_ROOT/crashes.log}"
CARGO_BIN="${CARGO:-cargo}"
RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-nightly}"
CARGO_FUZZ_RUN_ARGS="${CARGO_FUZZ_RUN_ARGS:---no-trace-compares}"
LIBFUZZER_ARGS="${LIBFUZZER_ARGS:-}"
ROTATELOGS_BIN="${ROTATELOGS_BIN:-}"

usage() {
    cat <<'EOF'
Usage:
  scripts/fuzz_screen.sh [options] [target-glob ...] [-- libFuzzer-arg ...]

Launch detached screen sessions for cargo-fuzz targets. With no target globs,
all [[bin]] targets in fuzz/Cargo.toml are launched.

Options:
  --list-targets       Print discovered fuzzer target names.
  --status             Print known target sessions and whether they are running.
  --stop               Stop matching screen sessions.
  --replace            Stop an existing matching session before launching it.
  --dry-run            Print launch commands without starting sessions.
  --prefix NAME        Screen session prefix. Default: evm2-fuzz.
  --log-root DIR       Log root. Default: target/fuzz-screen-logs.
  --rotate-size SIZE   rotatelogs size/time argument. Default: 100M.
  -h, --help           Show this help.

Environment:
  CARGO_FUZZ_RUN_ARGS  Extra cargo-fuzz run args. Default: --no-trace-compares
  LIBFUZZER_ARGS       Default libFuzzer args appended after '--'.
  FUZZ_CRASH_HOOK      Optional executable called on nonzero fuzzer exit.
                       Overrides the default Slack hook.
                       Args: target status current_log artifact_dir session
                       Env:  FUZZ_TARGET FUZZ_STATUS FUZZ_LOG
                             FUZZ_ARTIFACT_DIR FUZZ_SESSION FUZZ_ROOT
  FUZZ_LOG_ROOT        Same as --log-root.
  FUZZ_LOG_ROTATE_SIZE Same as --rotate-size.
  FUZZ_SCREEN_PREFIX   Same as --prefix.
  SLACK_WEBHOOK_URL    If set, scripts/slack_fuzz_crash_hook.sh is used by default.
  FUZZ_SCREEN_ENV_FILE Optional private env file. Default: .fuzz_screeen.env

Examples:
  scripts/fuzz_screen.sh
  scripts/fuzz_screen.sh 'bytecode_compare_*' -- -rss_limit_mb=8192
  SLACK_WEBHOOK_URL=<webhook-url> scripts/fuzz_screen.sh '*_amsterdam'
  FUZZ_CRASH_HOOK=./notify-crash.sh scripts/fuzz_screen.sh '*_amsterdam'
  CARGO_FUZZ_RUN_ARGS='--features jit --no-trace-compares' scripts/fuzz_screen.sh 'evm_smith_*'
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command '$1'"
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

resolve_rotatelogs() {
    if [[ -n "$ROTATELOGS_BIN" ]]; then
        printf '%s\n' "$ROTATELOGS_BIN"
        return
    fi
    if command -v rotatelogs >/dev/null 2>&1; then
        command -v rotatelogs
        return
    fi
    if command -v rotatelog >/dev/null 2>&1; then
        command -v rotatelog
        return
    fi
    die "missing rotatelogs; install apache2-bin or set ROTATELOGS_BIN"
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

session_name() {
    local safe
    safe="$(printf '%s' "$1" | tr -c '[:alnum:]_.-' '_')"
    printf '%s.%s\n' "$SESSION_PREFIX" "$safe"
}

regex_escape() {
    sed 's/[][\.|$(){}?+*^\\]/\\&/g' <<<"$1"
}

session_exists() {
    local session escaped
    session="$1"
    escaped="$(regex_escape "$session")"
    screen -ls | grep -Eq "[[:space:]][0-9]+\\.${escaped}[[:space:]]"
}

append_crash_record() {
    local target="$1"
    local status="$2"
    local current_log="$3"
    local artifact_dir="$4"
    local session="$5"

    mkdir -p "$(dirname -- "$CRASH_LOG")"
    {
        printf '[%s] target=%s status=%s session=%s\n' "$(date -Is)" "$target" "$status" "$session"
        printf '  log: %s\n' "$current_log"
        printf '  artifacts: %s\n' "$artifact_dir"
        find "$artifact_dir" -maxdepth 1 -type f -printf '  artifact: %TY-%Tm-%TdT%TH:%TM:%TS %p\n' 2>/dev/null \
            | sort \
            | tail -n 10 || true
    } >>"$CRASH_LOG"
}

configured_crash_hook() {
    if [[ -n "${FUZZ_CRASH_HOOK:-}" ]]; then
        printf "%s\n" "$FUZZ_CRASH_HOOK"
        return
    fi
    if [[ -n "${SLACK_WEBHOOK_URL:-}" && -x "$DEFAULT_CRASH_HOOK" ]]; then
        printf "%s\n" "$DEFAULT_CRASH_HOOK"
    fi
}

notify_crash() {
    local target="$1"
    local status="$2"
    local current_log="$3"
    local artifact_dir="$4"
    local session="$5"
    local message
    local crash_hook

    message="cargo-fuzz target $target exited with status $status; log: $current_log; artifacts: $artifact_dir"
    append_crash_record "$target" "$status" "$current_log" "$artifact_dir" "$session"

    crash_hook="$(configured_crash_hook)"
    if [[ -n "$crash_hook" ]]; then
        FUZZ_TARGET="$target" \
            FUZZ_STATUS="$status" \
            FUZZ_LOG="$current_log" \
            FUZZ_ARTIFACT_DIR="$artifact_dir" \
            FUZZ_SESSION="$session" \
            FUZZ_ROOT="$ROOT" \
            "$crash_hook" "$target" "$status" "$current_log" "$artifact_dir" "$session" \
            >>"$CRASH_LOG" 2>&1 || true
        return
    fi

    if command -v notify-send >/dev/null 2>&1 && [[ -n "${DISPLAY:-}" ]]; then
        notify-send "cargo-fuzz crash: $target" "$message" || true
    elif command -v wall >/dev/null 2>&1; then
        printf '%s\n' "$message" | wall -n >/dev/null 2>&1 || true
    fi
}

print_command() {
    printf 'command:'
    printf ' %q' "$@"
    printf '\n'
}

run_worker() {
    local target="$1"
    shift
    if [[ "${1:-}" == "--" ]]; then
        shift
    fi

    local session="${FUZZ_SCREEN_SESSION:-$(session_name "$target")}"
    local log_dir="$LOG_ROOT/$target"
    local current_log="$log_dir/current.log"
    local log_pattern="$log_dir/%Y%m%dT%H%M%S.log"
    local artifact_dir="$ROOT/fuzz/artifacts/$target"
    local rotatelogs_bin
    local cargo_args=()
    local env_libfuzzer_args=()
    local libfuzzer_args=()
    local status

    require_cmd "$CARGO_BIN"
    rotatelogs_bin="$(resolve_rotatelogs)"
    mkdir -p "$log_dir" "$artifact_dir" "$ROOT/fuzz/corpus/$target"

    split_words cargo_args "$CARGO_FUZZ_RUN_ARGS"
    split_words env_libfuzzer_args "$LIBFUZZER_ARGS"
    libfuzzer_args=("${env_libfuzzer_args[@]}" "$@")

    export RUST_BACKTRACE="${RUST_BACKTRACE:-full}"
    export RUST_LIB_BACKTRACE="${RUST_LIB_BACKTRACE:-full}"
    export UBSAN_OPTIONS="${UBSAN_OPTIONS:-print_stacktrace=1}"
    export ASAN_OPTIONS="${ASAN_OPTIONS:-detect_odr_violation=0:symbolize=1:abort_on_error=1:disable_coredump=0}"

    set +e
    {
        printf '[%s] starting target=%s session=%s\n' "$(date -Is)" "$target" "$session"
        printf 'root: %s\n' "$ROOT"
        printf 'log: %s\n' "$current_log"
        printf 'artifacts: %s\n' "$artifact_dir"
        print_command "$CARGO_BIN" "+$RUSTUP_TOOLCHAIN" fuzz run "${cargo_args[@]}" "$target" -- "${libfuzzer_args[@]}"
        "$CARGO_BIN" "+$RUSTUP_TOOLCHAIN" fuzz run "${cargo_args[@]}" "$target" -- "${libfuzzer_args[@]}"
    } 2>&1 | "$rotatelogs_bin" -L "$current_log" "$log_pattern" "$LOG_ROTATE_SIZE"
    status=${PIPESTATUS[0]}
    set -e

    printf '[%s] target=%s exited status=%s\n' "$(date -Is)" "$target" "$status" >>"$current_log"
    if ((status != 0)); then
        notify_crash "$target" "$status" "$current_log" "$artifact_dir" "$session"
    fi
    exit "$status"
}

list_status() {
    local target session state log
    while IFS= read -r target; do
        session="$(session_name "$target")"
        log="$LOG_ROOT/$target/current.log"
        if session_exists "$session"; then
            state="running"
        else
            state="stopped"
        fi
        printf '%-48s %-9s %s\n' "$target" "$state" "$log"
    done < <(select_targets "$@")
}

stop_sessions() {
    local target session
    while IFS= read -r target; do
        session="$(session_name "$target")"
        if session_exists "$session"; then
            printf 'stopping %s (%s)\n' "$target" "$session"
            screen -S "$session" -X quit
        fi
    done < <(select_targets "$@")
}

launch_sessions() {
    local replace="$1"
    local dry_run="$2"
    shift 2
    local libfuzzer_args=("$@")
    local target session command
    local launched=0

    require_cmd screen
    require_cmd "$CARGO_BIN"
    ROTATELOGS_BIN="$(resolve_rotatelogs)"
    mkdir -p "$LOG_ROOT"

    while IFS= read -r target; do
        session="$(session_name "$target")"
        if session_exists "$session"; then
            if ((replace)); then
                printf 'replacing %s (%s)\n' "$target" "$session"
                screen -S "$session" -X quit
            else
                printf 'skipping %s; session already exists (%s)\n' "$target" "$session"
                continue
            fi
        fi

        command=(
            env
            "FUZZ_SCREEN_SESSION=$session"
            "FUZZ_SCREEN_PREFIX=$SESSION_PREFIX"
            "FUZZ_LOG_ROOT=$LOG_ROOT"
            "FUZZ_LOG_ROTATE_SIZE=$LOG_ROTATE_SIZE"
            "FUZZ_CRASH_LOG=$CRASH_LOG"
            "FUZZ_SCREEN_ENV_FILE=$ENV_FILE"
            "ROTATELOGS_BIN=$ROTATELOGS_BIN"
            "CARGO=$CARGO_BIN"
            "RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN"
            "CARGO_FUZZ_RUN_ARGS=$CARGO_FUZZ_RUN_ARGS"
            "LIBFUZZER_ARGS=$LIBFUZZER_ARGS"
            bash "$SCRIPT" --worker "$target" --
            "${libfuzzer_args[@]}"
        )

        if ((dry_run)); then
            printf 'screen -dmS %q' "$session"
            printf ' %q' "${command[@]}"
            printf '\n'
        else
            printf 'launching %s (%s)\n' "$target" "$session"
            screen -dmS "$session" "${command[@]}"
        fi
        launched=$((launched + 1))
    done < <(select_targets "${TARGET_FILTERS[@]}")

    if ((launched == 0)); then
        die "no matching fuzzer targets"
    fi
}

if [[ "${1:-}" == "--worker" ]]; then
    shift
    [[ $# -ge 1 ]] || die "--worker requires a target"
    run_worker "$@"
fi

ACTION="start"
REPLACE=0
DRY_RUN=0
TARGET_FILTERS=()
CLI_LIBFUZZER_ARGS=()

while (($#)); do
    case "$1" in
        --list-targets)
            ACTION="list"
            shift
            ;;
        --status)
            ACTION="status"
            shift
            ;;
        --stop)
            ACTION="stop"
            shift
            ;;
        --replace)
            REPLACE=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --prefix)
            [[ $# -ge 2 ]] || die "--prefix requires a value"
            SESSION_PREFIX="$2"
            shift 2
            ;;
        --log-root)
            [[ $# -ge 2 ]] || die "--log-root requires a value"
            LOG_ROOT="$2"
            CRASH_LOG="${FUZZ_CRASH_LOG:-$LOG_ROOT/crashes.log}"
            shift 2
            ;;
        --rotate-size)
            [[ $# -ge 2 ]] || die "--rotate-size requires a value"
            LOG_ROTATE_SIZE="$2"
            shift 2
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

case "$ACTION" in
    list)
        select_targets "${TARGET_FILTERS[@]}"
        ;;
    status)
        require_cmd screen
        list_status "${TARGET_FILTERS[@]}"
        ;;
    stop)
        require_cmd screen
        stop_sessions "${TARGET_FILTERS[@]}"
        ;;
    start)
        launch_sessions "$REPLACE" "$DRY_RUN" "${CLI_LIBFUZZER_ARGS[@]}"
        ;;
    *)
        die "unknown action '$ACTION'"
        ;;
esac
