#!/usr/bin/env bash
set -euo pipefail

target="${1:-${FUZZ_TARGET:-unknown}}"
status="${2:-${FUZZ_STATUS:-unknown}}"
current_log="${3:-${FUZZ_LOG:-}}"
artifact_dir="${4:-${FUZZ_ARTIFACT_DIR:-}}"
session="${5:-${FUZZ_SESSION:-}}"
root="${FUZZ_ROOT:-$(pwd)}"
tail_lines="${SLACK_LOG_TAIL_LINES:-40}"
tail_bytes="${SLACK_LOG_TAIL_BYTES:-3500}"

if [[ -z "${SLACK_WEBHOOK_URL:-}" ]]; then
    echo "SLACK_WEBHOOK_URL is not set; skipping Slack notification" >&2
    exit 0
fi

latest_artifact=""
if [[ -n "$artifact_dir" && -d "$artifact_dir" ]]; then
    latest_artifact="$(
        find "$artifact_dir" -maxdepth 1 -type f -printf '%T@ %p\n' 2>/dev/null \
            | sort -nr \
            | head -n 1 \
            | cut -d' ' -f2- || true
    )"
fi

log_tail=""
if [[ -n "$current_log" && -f "$current_log" ]]; then
    log_tail="$(tail -n "$tail_lines" "$current_log" 2>/dev/null | tail -c "$tail_bytes" || true)"
fi

host="$(hostname -f 2>/dev/null || hostname 2>/dev/null || printf unknown)"

payload="$(
    python3 - "$target" "$status" "$current_log" "$artifact_dir" "$session" "$root" "$latest_artifact" "$host" "$log_tail" <<'PY'
import json
import sys

target, status, log, artifacts, session, root, latest_artifact, host, log_tail = sys.argv[1:]

summary = f"cargo-fuzz target `{target}` exited with status `{status}` on `{host}`"
fields = [
    f"*Target:*\n`{target}`",
    f"*Status:*\n`{status}`",
    f"*Session:*\n`{session or 'unknown'}`",
    f"*Root:*\n`{root}`",
    f"*Log:*\n`{log or 'unknown'}`",
    f"*Artifacts:*\n`{artifacts or 'unknown'}`",
]
if latest_artifact:
    fields.append(f"*Latest artifact:*\n`{latest_artifact}`")

blocks = [
    {
        "type": "section",
        "text": {"type": "mrkdwn", "text": f":rotating_light: {summary}"},
    },
    {
        "type": "section",
        "fields": [{"type": "mrkdwn", "text": field} for field in fields],
    },
]
if log_tail:
    blocks.extend(
        [
            {"type": "divider"},
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": "*Log tail:*\n```" + log_tail[-3000:] + "```",
                },
            },
        ]
    )

print(json.dumps({"text": summary, "blocks": blocks}))
PY
)"

curl -fsS \
    -X POST \
    -H 'Content-type: application/json' \
    --data "$payload" \
    "$SLACK_WEBHOOK_URL"
