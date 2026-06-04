import json
import os
import queue
import resource
import subprocess
import sys
import threading
import time
from pathlib import Path

import modal

APP_NAME = "evm2-replays"
VOLUME_NAME = os.environ.get("EVM2_MODAL_VOLUME", "evm2-replays")
RPC_URL = os.environ.get("EVM2_RPC_URL", "https://ethereum.reth.rs/rpc")

DATA_DIR = Path("/data")
FIXTURE_DIR = DATA_DIR / "fixtures"
LOG_DIR = DATA_DIR / "logs"
RESULT_DIR = DATA_DIR / "results"
EVM2 = "/usr/local/bin/evm2"

DEFAULT_CAPTURE_CPU = 2.0
DEFAULT_REPLAY_CPU = 1.0
DEFAULT_MEMORY_MB = 2048
DEFAULT_MAX_CONTAINERS = 8
OK_STATUSES = {"ok", "exists"}

app = modal.App(APP_NAME)
volume = modal.Volume.from_name(VOLUME_NAME, create_if_missing=True)
image = modal.Image.debian_slim(python_version="3.12").add_local_file(
    "target/x86_64-unknown-linux-musl/release/evm2",
    EVM2,
    copy=True,
)


def range_end(start: int, blocks: int) -> int:
    return start + blocks - 1


def range_slug(start: int, blocks: int) -> str:
    return f"{start}-{range_end(start, blocks)}"


def fixture_path(start: int, blocks: int) -> Path:
    return FIXTURE_DIR / f"mainnet_{start}_{range_end(start, blocks)}.json"


def log_path(stage: str, slug: str) -> Path:
    return LOG_DIR / f"{stage}-{slug}.log"


def result_path(stage: str, slug: str) -> Path:
    return RESULT_DIR / stage / f"{slug}.json"


def ensure_dirs() -> None:
    for path in [
        FIXTURE_DIR,
        LOG_DIR,
        RESULT_DIR / "capture",
        RESULT_DIR / "replay",
        RESULT_DIR / "pipeline",
    ]:
        path.mkdir(parents=True, exist_ok=True)


def child_peak_rss_kib() -> int:
    rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    if sys.platform == "darwin":
        rss //= 1024
    return int(rss)


def stream_reader(stream_name: str, pipe, events: queue.Queue) -> None:
    try:
        for line in pipe:
            events.put((stream_name, line))
    finally:
        events.put((stream_name, None))


def run_process(args: list[str], label: str, heartbeat_sec: float = 30.0) -> dict:
    started = time.time()
    process = subprocess.Popen(
        args,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=1,
    )
    print_json({"stage": "process-start", "label": label, "pid": process.pid})

    events: queue.Queue = queue.Queue()
    readers = [
        threading.Thread(target=stream_reader, args=("stdout", process.stdout, events), daemon=True),
        threading.Thread(target=stream_reader, args=("stderr", process.stderr, events), daemon=True),
    ]
    for reader in readers:
        reader.start()

    stdout_lines: list[str] = []
    stderr_lines: list[str] = []
    open_streams = len(readers)
    last_heartbeat = started

    while open_streams:
        try:
            stream_name, line = events.get(timeout=1.0)
        except queue.Empty:
            now = time.time()
            if now - last_heartbeat >= heartbeat_sec:
                print_json({"stage": "process-heartbeat", "label": label, "elapsed_sec": now - started})
                last_heartbeat = now
            continue

        if line is None:
            open_streams -= 1
            continue

        if stream_name == "stdout":
            stdout_lines.append(line)
        else:
            stderr_lines.append(line)
        print(f"[{label}] {line}", end="", flush=True)

    returncode = process.wait()
    elapsed_sec = time.time() - started
    print_json({"stage": "process-exit", "label": label, "returncode": returncode, "elapsed_sec": elapsed_sec})
    return {
        "returncode": returncode,
        "stdout": "".join(stdout_lines),
        "stderr": "".join(stderr_lines),
        "elapsed_sec": elapsed_sec,
        "peak_rss_kib": child_peak_rss_kib(),
    }


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def print_json(value: object) -> None:
    print(json.dumps(value, sort_keys=True), flush=True)


def shard_plan(start: int, total_blocks: int, shard_blocks: int) -> list[dict]:
    if total_blocks <= 0:
        raise ValueError("total_blocks must be positive")
    if shard_blocks <= 0:
        raise ValueError("shard_blocks must be positive")

    shards = []
    remaining = total_blocks
    shard_start = start
    index = 0
    while remaining:
        blocks = min(shard_blocks, remaining)
        shards.append({"label": f"shard-{index:05d}", "start": shard_start, "blocks": blocks})
        shard_start += blocks
        remaining -= blocks
        index += 1
    return shards


def base_result(stage: str, start: int, blocks: int, label: str | None) -> dict:
    return {
        "stage": stage,
        "label": label,
        "start": start,
        "end": range_end(start, blocks),
        "blocks": blocks,
    }


def persist_result(stage: str, slug: str, result: dict) -> None:
    write_json(result_path(stage, slug), result)
    volume.commit()


def capture_impl(start: int, blocks: int, rpc_url: str, force: bool, label: str | None) -> dict:
    volume.reload()
    ensure_dirs()

    slug = range_slug(start, blocks)
    fixture = fixture_path(start, blocks)
    result = base_result("capture", start, blocks, label)
    result["fixture"] = str(fixture)

    if fixture.exists() and not force:
        result["status"] = "exists"
        result["fixture_bytes"] = fixture.stat().st_size
        persist_result("capture", slug, result)
        return result

    tmp_fixture = fixture.with_suffix(".json.tmp")
    if tmp_fixture.exists():
        tmp_fixture.unlink()

    log = log_path("capture", slug)
    process = run_process(
        [EVM2, "capture", "--rpc", rpc_url, "--range", slug, "--output", str(tmp_fixture)],
        f"capture-{slug}",
    )
    log.write_text(process["stdout"] + process["stderr"])

    result.update(
        {
            "status": "ok" if process["returncode"] == 0 else "failed",
            "returncode": process["returncode"],
            "elapsed_sec": process["elapsed_sec"],
            "peak_rss_kib": process["peak_rss_kib"],
            "log": str(log),
        }
    )
    if process["returncode"] == 0:
        tmp_fixture.replace(fixture)
        result["fixture_bytes"] = fixture.stat().st_size
    elif tmp_fixture.exists():
        tmp_fixture.unlink()

    persist_result("capture", slug, result)
    return result


def replay_impl(start: int, blocks: int, label: str | None) -> dict:
    volume.reload()
    ensure_dirs()

    slug = range_slug(start, blocks)
    fixture = fixture_path(start, blocks)
    if not fixture.exists():
        result = base_result("replay", start, blocks, label)
        result.update({"status": "missing", "fixture": str(fixture)})
        persist_result("replay", slug, result)
        return result

    log = log_path("replay", slug)
    process = run_process([EVM2, "replay", str(fixture)], f"replay-{slug}")
    log.write_text(process["stdout"] + process["stderr"])

    result = base_result("replay", start, blocks, label)
    result.update(
        {
            "status": "ok" if process["returncode"] == 0 else "failed",
            "returncode": process["returncode"],
            "elapsed_sec": process["elapsed_sec"],
            "peak_rss_kib": process["peak_rss_kib"],
            "fixture": str(fixture),
            "fixture_bytes": fixture.stat().st_size,
            "log": str(log),
        }
    )
    persist_result("replay", slug, result)
    return result


@app.function(
    image=image,
    volumes={DATA_DIR: volume},
    timeout=24 * 60 * 60,
    cpu=DEFAULT_CAPTURE_CPU,
    memory=DEFAULT_MEMORY_MB,
    max_containers=DEFAULT_MAX_CONTAINERS,
    retries=2,
)
def capture_shard(start: int, blocks: int, rpc_url: str, force: bool, label: str | None) -> dict:
    return capture_impl(start, blocks, rpc_url, force, label)


@app.function(
    image=image,
    volumes={DATA_DIR: volume},
    timeout=24 * 60 * 60,
    cpu=DEFAULT_REPLAY_CPU,
    memory=DEFAULT_MEMORY_MB,
    max_containers=DEFAULT_MAX_CONTAINERS,
    retries=1,
)
def replay_shard(start: int, blocks: int, label: str | None) -> dict:
    return replay_impl(start, blocks, label)


@app.function(
    image=image,
    volumes={DATA_DIR: volume},
    timeout=24 * 60 * 60,
    cpu=DEFAULT_CAPTURE_CPU,
    memory=DEFAULT_MEMORY_MB,
    max_containers=DEFAULT_MAX_CONTAINERS,
    retries=1,
)
def capture_then_replay_shard(
    start: int,
    blocks: int,
    rpc_url: str,
    force: bool,
    label: str | None,
) -> dict:
    slug = range_slug(start, blocks)
    capture_result = capture_impl(start, blocks, rpc_url, force, label)
    if capture_result["status"] not in OK_STATUSES:
        result = base_result("pipeline", start, blocks, label)
        result.update({"status": "failed", "capture": capture_result, "replay": None})
        persist_result("pipeline", slug, result)
        return result

    replay_result = replay_impl(start, blocks, label)
    result = base_result("pipeline", start, blocks, label)
    result.update(
        {
            "status": "ok" if replay_result["status"] == "ok" else "failed",
            "capture": capture_result,
            "replay": replay_result,
        }
    )
    persist_result("pipeline", slug, result)
    return result


def print_result(result: object) -> tuple[str, dict | None]:
    if isinstance(result, BaseException):
        value = {"status": "exception", "error": repr(result)}
        print_json(value)
        return "exception", None

    print_json(result)
    if isinstance(result, dict):
        return str(result.get("status", "unknown")), result
    return "unknown", None


def overall_status(statuses: dict[str, int]) -> str:
    return "ok" if all(status in OK_STATUSES for status in statuses) else "failed"


def print_summary(stage: str, shards: list[dict], statuses: dict[str, int]) -> str:
    first = shards[0]
    last = shards[-1]
    status = overall_status(statuses)
    print_json(
        {
            "stage": f"{stage}-summary",
            "status": status,
            "shards": len(shards),
            "total_blocks": sum(shard["blocks"] for shard in shards),
            "first_range": range_slug(first["start"], first["blocks"]),
            "last_range": range_slug(last["start"], last["blocks"]),
            "statuses": statuses,
        }
    )
    return status


def finish(stage: str, shards: list[dict], statuses: dict[str, int]) -> None:
    if print_summary(stage, shards, statuses) != "ok":
        raise SystemExit(1)


@app.local_entrypoint()
def capture(
    start: int,
    total_blocks: int,
    shard_blocks: int = 250,
    force: bool = False,
    max_containers: int = DEFAULT_MAX_CONTAINERS,
    memory_mb: int = DEFAULT_MEMORY_MB,
    cpu: float = DEFAULT_CAPTURE_CPU,
    rpc_url: str = RPC_URL,
) -> None:
    shards = shard_plan(start, total_blocks, shard_blocks)
    fn = capture_shard.with_options(cpu=cpu, memory=memory_mb, max_containers=max_containers)
    statuses: dict[str, int] = {}
    for result in fn.map(
        [shard["start"] for shard in shards],
        [shard["blocks"] for shard in shards],
        [rpc_url] * len(shards),
        [force] * len(shards),
        [shard["label"] for shard in shards],
        order_outputs=False,
        return_exceptions=True,
    ):
        status, _ = print_result(result)
        statuses[status] = statuses.get(status, 0) + 1
    finish("capture", shards, statuses)


@app.local_entrypoint()
def replay(
    start: int,
    total_blocks: int,
    shard_blocks: int = 250,
    max_containers: int = DEFAULT_MAX_CONTAINERS,
    memory_mb: int = DEFAULT_MEMORY_MB,
    cpu: float = DEFAULT_REPLAY_CPU,
) -> None:
    shards = shard_plan(start, total_blocks, shard_blocks)
    fn = replay_shard.with_options(cpu=cpu, memory=memory_mb, max_containers=max_containers)
    statuses: dict[str, int] = {}
    for result in fn.map(
        [shard["start"] for shard in shards],
        [shard["blocks"] for shard in shards],
        [shard["label"] for shard in shards],
        order_outputs=False,
        return_exceptions=True,
    ):
        status, _ = print_result(result)
        statuses[status] = statuses.get(status, 0) + 1
    finish("replay", shards, statuses)


@app.local_entrypoint()
def pipeline(
    start: int,
    total_blocks: int,
    shard_blocks: int = 250,
    force: bool = False,
    max_containers: int = DEFAULT_MAX_CONTAINERS,
    memory_mb: int = DEFAULT_MEMORY_MB,
    cpu: float = DEFAULT_CAPTURE_CPU,
    rpc_url: str = RPC_URL,
) -> None:
    shards = shard_plan(start, total_blocks, shard_blocks)
    fn = capture_then_replay_shard.with_options(cpu=cpu, memory=memory_mb, max_containers=max_containers)
    statuses: dict[str, int] = {}
    for result in fn.map(
        [shard["start"] for shard in shards],
        [shard["blocks"] for shard in shards],
        [rpc_url] * len(shards),
        [force] * len(shards),
        [shard["label"] for shard in shards],
        order_outputs=False,
        return_exceptions=True,
    ):
        status, _ = print_result(result)
        statuses[status] = statuses.get(status, 0) + 1
    finish("pipeline", shards, statuses)


def parse_fixture_range(path: str) -> tuple[int, int] | None:
    name = Path(path).name
    if not name.startswith("mainnet_") or not name.endswith(".json"):
        return None
    start, end = name.removeprefix("mainnet_").removesuffix(".json").split("_", 1)
    return int(start), int(end)


@app.local_entrypoint()
def list_fixtures(limit: int = 0) -> None:
    entries = sorted(entry.path for entry in volume.listdir("/fixtures", recursive=False))
    fixtures = []
    for path in entries:
        parsed = parse_fixture_range(path)
        if parsed is None:
            continue
        start, end = parsed
        fixtures.append({"path": f"/{path}", "start": start, "end": end, "blocks": end - start + 1})

    visible = fixtures[:limit] if limit > 0 else fixtures
    print_json(
        {
            "stage": "list-fixtures",
            "count": len(fixtures),
            "fixture_blocks": sum(fixture["blocks"] for fixture in fixtures),
            "fixtures": visible,
        }
    )
