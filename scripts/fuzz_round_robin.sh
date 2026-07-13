#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/fuzz_round_robin.sh"
SCREEN_SCRIPT="$ROOT/scripts/fuzz_screen.sh"
WORKER_SCRIPT="${FUZZ_ROUND_ROBIN_WORKER_SCRIPT:-$SCREEN_SCRIPT}"

SESSION_NAME="${FUZZ_ROUND_ROBIN_SESSION:-evm2-fuzz.round-robin}"
STATE_ROOT="${FUZZ_ROUND_ROBIN_STATE_ROOT:-$ROOT/target/fuzz-round-robin}"
STATE_FILE="$STATE_ROOT/state"
SCHEDULER_LOG="$STATE_ROOT/scheduler.log"
JOBS="${FUZZ_ROUND_ROBIN_JOBS:-8}"
SLICE_SECONDS="${FUZZ_ROUND_ROBIN_SLICE:-3600}"

usage() {
    cat <<'EOF'
Usage:
  scripts/fuzz_round_robin.sh [options] [target-glob ...] [-- libFuzzer-arg ...]

Run cargo-fuzz targets continuously in bounded parallel batches. Each target runs
for one time slice, then the scheduler advances to the next target. With no target
globs, all targets in fuzz/Cargo.toml participate.

Options:
  --status           Print scheduler state and active targets.
  --stop             Stop the scheduler and its active target processes.
  --replace          Replace an existing scheduler session.
  --jobs N           Concurrent targets. Default: 8.
  --slice SECONDS    Time per target before rotation. Default: 3600.
  --session NAME     Screen session name. Default: evm2-fuzz.round-robin.
  --state-root DIR   Scheduler state/log directory.
  --dry-run          Print the launch command without starting it.
  -h, --help         Show this help.

Environment:
  FUZZ_ROUND_ROBIN_JOBS        Same as --jobs.
  FUZZ_ROUND_ROBIN_SLICE       Same as --slice.
  FUZZ_ROUND_ROBIN_SESSION     Same as --session.
  FUZZ_ROUND_ROBIN_STATE_ROOT  Same as --state-root.

The scheduler enforces each slice as an external wall-clock limit and uses an
effectively unreachable slow-unit threshold. Other libFuzzer defaults come from scripts/fuzz_screen.sh, currently -timeout=300,
-ignore_ooms=1, and -rss_limit_mb=8192.
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command '$1'"
}

require_positive_integer() {
    local name="$1"
    local value="$2"
    [[ "$value" =~ ^[1-9][0-9]*$ ]] || die "$name must be a positive integer"
}

regex_escape() {
    sed 's/[][\.|$(){}?+*^\\]/\\&/g' <<<"$1"
}

session_exists() {
    local escaped
    escaped="$(regex_escape "$SESSION_NAME")"
    screen -ls | grep -Eq "[[:space:]][0-9]+\\.${escaped}[[:space:]]"
}

state_pid() {
    [[ -f "$STATE_FILE" ]] || return 0

    local key value
    while IFS='=' read -r key value; do
        if [[ "$key" == pid ]]; then
            printf '%s\n' "$value"
            return
        fi
    done <"$STATE_FILE"
}
ACTIVE_PIDS=()
ACTIVE_TARGETS=()
CURRENT_CYCLE=0
CURRENT_BATCH=0
CURRENT_STARTED=""
SCHEDULER_STARTED=""
SELECTED_TARGETS=()

write_state() {
    local status="$1"
    local tmp="$STATE_FILE.tmp.$$"
    mkdir -p "$STATE_ROOT"
    {
        printf 'status=%s\n' "$status"
        printf 'pid=%s\n' "$$"
        printf 'session=%s\n' "$SESSION_NAME"
        printf 'jobs=%s\n' "$JOBS"
        printf 'slice_seconds=%s\n' "$SLICE_SECONDS"
        printf 'scheduler_started=%s\n' "$SCHEDULER_STARTED"
        printf 'cycle=%s\n' "$CURRENT_CYCLE"
        printf 'batch=%s\n' "$CURRENT_BATCH"
        printf 'batch_started=%s\n' "$CURRENT_STARTED"
        printf 'target_count=%s\n' "${#SELECTED_TARGETS[@]}"
        local target
        for target in "${ACTIVE_TARGETS[@]}"; do
            printf 'active=%s\n' "$target"
        done
    } >"$tmp"
    mv "$tmp" "$STATE_FILE"
}

terminate_children() {
    local pid
    for pid in "${ACTIVE_PIDS[@]}"; do
        pkill -TERM -s "$pid" '.*' 2>/dev/null || true
    done

    local attempt session_alive
    for attempt in {1..30}; do
        session_alive=0
        for pid in "${ACTIVE_PIDS[@]}"; do
            if pgrep -s "$pid" '.*' >/dev/null 2>&1; then
                session_alive=1
                break
            fi
        done
        ((session_alive == 0)) && break
        sleep 0.1
    done

    for pid in "${ACTIVE_PIDS[@]}"; do
        pkill -KILL -s "$pid" '.*' 2>/dev/null || true
    done
    for pid in "${ACTIVE_PIDS[@]}"; do
        wait "$pid" 2>/dev/null || true
    done
    ACTIVE_PIDS=()
    ACTIVE_TARGETS=()
}

scheduler_exit() {
    local status=$?
    trap - EXIT HUP INT TERM
    terminate_children
    local owner_pid
    owner_pid="$(state_pid)"
    if [[ -z "$owner_pid" || "$owner_pid" == "$$" ]]; then
        write_state stopped
    fi
    printf '[%s] scheduler stopped status=%s\n' "$(date -Is)" "$status"
    exit "$status"
}

run_scheduler() {
    require_cmd setsid
    require_cmd pgrep
    require_cmd pkill
    [[ -f "$WORKER_SCRIPT" ]] || die "worker script not found: $WORKER_SCRIPT"
    mapfile -t SELECTED_TARGETS < <("$SCREEN_SCRIPT" --list-targets "${TARGET_FILTERS[@]}")
    (("${#SELECTED_TARGETS[@]}" > 0)) || die "no matching fuzzer targets"

    mkdir -p "$STATE_ROOT"
    exec >>"$SCHEDULER_LOG" 2>&1

    SCHEDULER_STARTED="$(date -Is)"
    trap scheduler_exit EXIT
    trap 'exit 0' HUP INT TERM
    printf '[%s] scheduler started jobs=%s slice=%ss targets=%s\n' \
        "$SCHEDULER_STARTED" "$JOBS" "$SLICE_SECONDS" "${#SELECTED_TARGETS[@]}"

    local offset target pid index status
    while :; do
        CURRENT_CYCLE=$((CURRENT_CYCLE + 1))
        CURRENT_BATCH=0
        for ((offset = 0; offset < ${#SELECTED_TARGETS[@]}; offset += JOBS)); do
            CURRENT_BATCH=$((CURRENT_BATCH + 1))
            CURRENT_STARTED="$(date -Is)"
            ACTIVE_TARGETS=("${SELECTED_TARGETS[@]:offset:JOBS}")
            ACTIVE_PIDS=()
            write_state running
            printf '[%s] cycle=%s batch=%s active=%s\n' \
                "$CURRENT_STARTED" "$CURRENT_CYCLE" "$CURRENT_BATCH" "${ACTIVE_TARGETS[*]}"

            for target in "${ACTIVE_TARGETS[@]}"; do
                FUZZ_SCREEN_SESSION="$SESSION_NAME.$target" \
                    FUZZ_WORKER_WALL_TIME="$SLICE_SECONDS" \
                    setsid bash "$WORKER_SCRIPT" --worker "$target" -- \
                    -report_slow_units=2147483647 \
                    "${CLI_LIBFUZZER_ARGS[@]}" &
                ACTIVE_PIDS+=("$!")
            done

            for index in "${!ACTIVE_PIDS[@]}"; do
                pid="${ACTIVE_PIDS[$index]}"
                target="${ACTIVE_TARGETS[$index]}"
                if wait "$pid"; then
                    status=0
                else
                    status=$?
                fi
                if ((status != 0)); then
                    printf '[%s] target=%s exited status=%s; continuing rotation\n' \
                        "$(date -Is)" "$target" "$status"
                fi
            done
            ACTIVE_PIDS=()
            ACTIVE_TARGETS=()
        done
    done
}

print_status() {
    require_cmd screen
    if session_exists; then
        printf 'scheduler running session=%s\n' "$SESSION_NAME"
    else
        printf 'scheduler stopped session=%s\n' "$SESSION_NAME"
    fi
    printf 'state: %s\n' "$STATE_FILE"
    printf 'log: %s\n' "$SCHEDULER_LOG"
    if [[ -f "$STATE_FILE" ]]; then
        sed 's/^active=/active: /' "$STATE_FILE"
    fi
}

stop_scheduler() {
    require_cmd screen
    if ! session_exists; then
        printf 'scheduler already stopped (%s)\n' "$SESSION_NAME"
        return
    fi

    local scheduler_pid
    scheduler_pid="$(state_pid)"
    printf 'stopping scheduler (%s)\n' "$SESSION_NAME"
    screen -S "$SESSION_NAME" -X quit

    local attempt
    for attempt in {1..100}; do
        if ! session_exists; then
            if [[ -z "$scheduler_pid" ]] || ! kill -0 "$scheduler_pid" 2>/dev/null; then
                return
            fi
        fi
        sleep 0.1
    done
    die "scheduler session did not stop cleanly"
}

launch_scheduler() {
    require_cmd screen
    require_cmd setsid
    local selected=()
    mapfile -t selected < <("$SCREEN_SCRIPT" --list-targets "${TARGET_FILTERS[@]}")
    (("${#selected[@]}" > 0)) || die "no matching fuzzer targets"

    if session_exists; then
        if ((REPLACE)); then
            stop_scheduler
        else
            die "scheduler already running ($SESSION_NAME); use --replace"
        fi
    fi

    mkdir -p "$STATE_ROOT"
    local command=(
        bash "$SCRIPT" --scheduler-worker
        --jobs "$JOBS"
        --slice "$SLICE_SECONDS"
        --session "$SESSION_NAME"
        --state-root "$STATE_ROOT"
        "${TARGET_FILTERS[@]}"
        --
        "${CLI_LIBFUZZER_ARGS[@]}"
    )
    if ((DRY_RUN)); then
        printf 'screen -dmS %q' "$SESSION_NAME"
        printf ' %q' "${command[@]}"
        printf '\n'
        return
    fi

    screen -dmS "$SESSION_NAME" "${command[@]}"
    printf 'launched scheduler session=%s jobs=%s slice=%ss targets=%s\n' \
        "$SESSION_NAME" "$JOBS" "$SLICE_SECONDS" "${#selected[@]}"
}

ACTION=start
REPLACE=0
DRY_RUN=0
TARGET_FILTERS=()
CLI_LIBFUZZER_ARGS=()

while (($#)); do
    case "$1" in
        --status)
            ACTION=status
            shift
            ;;
        --stop)
            ACTION=stop
            shift
            ;;
        --replace)
            REPLACE=1
            shift
            ;;
        --scheduler-worker)
            ACTION=worker
            shift
            ;;
        --jobs)
            [[ $# -ge 2 ]] || die "--jobs requires a value"
            JOBS="$2"
            shift 2
            ;;
        --slice)
            [[ $# -ge 2 ]] || die "--slice requires a value"
            SLICE_SECONDS="$2"
            shift 2
            ;;
        --session)
            [[ $# -ge 2 ]] || die "--session requires a value"
            SESSION_NAME="$2"
            shift 2
            ;;
        --state-root)
            [[ $# -ge 2 ]] || die "--state-root requires a value"
            STATE_ROOT="$2"
            STATE_FILE="$STATE_ROOT/state"
            SCHEDULER_LOG="$STATE_ROOT/scheduler.log"
            shift 2
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

require_positive_integer jobs "$JOBS"
require_positive_integer slice "$SLICE_SECONDS"

case "$ACTION" in
    start)
        launch_scheduler
        ;;
    status)
        print_status
        ;;
    stop)
        stop_scheduler
        ;;
    worker)
        run_scheduler
        ;;
    *)
        die "unknown action '$ACTION'"
        ;;
esac
