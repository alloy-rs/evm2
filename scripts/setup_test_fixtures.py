#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "httpx>=0.28",
#     "rich>=15",
# ]
# ///
"""Download Ethereum Execution Spec Test fixtures."""

from __future__ import annotations

import asyncio
import os
import sys
from collections.abc import Awaitable, Callable
from contextlib import suppress
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

import httpx
from rich.console import Console
from rich.progress import (
    BarColumn,
    Progress,
    ProgressColumn,
    SpinnerColumn,
    TaskProgressColumn,
    TextColumn,
    TimeRemainingColumn,
    TransferSpeedColumn,
)
from rich.text import Text

MAIN_VERSION = os.environ.get("MAIN_VERSION", "v5.3.0")
# ethereum/legacytests commit pinned by the `LegacyTests` submodule of ethereum/tests v17.2. The
# ethereum/tests archives do not include submodules, so the tests are fetched from the submodule's
# own repository.
LEGACY_VERSION = os.environ.get("LEGACY_VERSION", "1f581b8ccdc4c63acf5f2c5c1b155c690c32a8eb")
BASE_URL = "https://github.com/ethereum/execution-spec-tests/releases/download"
# Mainline execution-specs fixtures cover Frontier..Amsterdam. The release is a URL path component, so
# the `@` is percent-encoded. Override EEST_VERSION/EEST_TAR/EEST_BASE_URL to select another release,
# for example a fork-specific devnet release.
EEST_BASE_URL = os.environ.get(
    "EEST_BASE_URL",
    "https://github.com/ethereum/execution-specs/releases/download",
)
DEFAULT_EEST_VERSION = "tests%40v21.0.1"
DEFAULT_EEST_TAR = "fixtures.tar.gz"
LEGACY_URL = f"https://github.com/ethereum/legacytests/archive/{LEGACY_VERSION}.tar.gz"

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
FIXTURES_DIR = Path(os.environ.get("EVM2_TEST_FIXTURES", REPO_ROOT / "test-fixtures"))

MAIN_STABLE_DIR = FIXTURES_DIR / "main" / "stable"
MAIN_DEVELOP_DIR = FIXTURES_DIR / "main" / "develop"
EEST_DIR = FIXTURES_DIR / "eest"
LEGACY_DIR = FIXTURES_DIR / "legacytests"

console = Console()


def human_bytes(size: float) -> str:
    for unit in ["bytes", "KB", "MB", "GB"]:
        if size < 1000 or unit == "GB":
            if unit == "bytes":
                return f"{size:.0f} {unit}"
            return f"{size:.1f} {unit}"
        size /= 1000
    raise AssertionError("unreachable")


class DownloadStatusColumn(ProgressColumn):
    def render(self, task) -> Text:
        completed = human_bytes(task.completed)
        if task.total is None:
            return Text(completed)
        return Text(f"{completed}/{human_bytes(task.total)}")


@dataclass(frozen=True)
class Fixture:
    label: str
    url: str
    dest: Path
    exists_paths: tuple[Path, ...]
    # tar member patterns to skip while extracting.
    excludes: tuple[str, ...] = ()


def env_flag(name: str) -> bool:
    value = os.environ.get(name)
    return value is not None and value != "" and value != "0"


def main_fixture() -> Fixture:
    if env_flag("EVM2_EEST_STABLE"):
        dest = MAIN_STABLE_DIR
        tar_name = os.environ.get("MAIN_TAR", "fixtures_stable.tar.gz")
        label = "main stable"
    else:
        dest = MAIN_DEVELOP_DIR
        tar_name = os.environ.get("MAIN_TAR", "fixtures_develop.tar.gz")
        label = "main develop"

    return Fixture(
        label=label,
        url=f"{BASE_URL}/{MAIN_VERSION}/{tar_name}",
        dest=dest,
        exists_paths=(dest / "state_tests", dest / "blockchain_tests"),
    )


def legacy_fixture() -> Fixture:
    return Fixture(
        label="legacy",
        url=LEGACY_URL,
        dest=LEGACY_DIR,
        exists_paths=(
            LEGACY_DIR / "Cancun" / "GeneralStateTests",
            LEGACY_DIR / "Constantinople" / "GeneralStateTests",
        ),
    )


def eest_fixture() -> Fixture | None:
    version = os.environ.get("EEST_VERSION", DEFAULT_EEST_VERSION)
    tar_name = os.environ.get("EEST_TAR", DEFAULT_EEST_TAR)

    # An explicitly cleared version or tar disables the execution-specs fixtures.
    if not version or not tar_name:
        return None

    return Fixture(
        label="eest",
        url=f"{EEST_BASE_URL}/{version}/{tar_name}",
        dest=EEST_DIR,
        exists_paths=(EEST_DIR / "state_tests", EEST_DIR / "blockchain_tests"),
        # The runner executes only the state and blockchain test formats.
        excludes=("*/blockchain_tests_engine*", "*/blockchain_tests_sync", "*/transaction_tests"),
    )


def fixture_plan() -> list[Fixture]:
    fixtures: list[Fixture] = []

    # The execution-specs release covers frontier..amsterdam, superseding the main develop suite by
    # fork coverage, so main develop/stable is opt-in via EVM2_STATETEST_MAIN.
    if env_flag("EVM2_STATETEST_MAIN"):
        fixtures.append(main_fixture())

    # Legacy hand-written GeneralStateTests, unless an execution-specs-only run was requested.
    if not env_flag("EVM2_STATETEST_EEST_ONLY"):
        fixtures.append(legacy_fixture())

    # Mainline execution-specs fixtures: the default suite.
    if eest := eest_fixture():
        fixtures.append(eest)

    return fixtures


async def retry(label: str, action: Callable[[], Awaitable[None]]) -> None:
    delay = 2
    for attempt in range(1, 6):
        try:
            await action()
            return
        except Exception as err:
            if attempt == 5:
                raise
            console.print(
                f"  {label}: attempt {attempt} failed: {short_error(err)}. "
                f"Retrying in {delay}s..."
            )
            await asyncio.sleep(delay)
            delay *= 2


def short_error(error: Exception) -> str:
    if isinstance(error, httpx.HTTPStatusError):
        return f"{error.response.status_code} {error.response.reason_phrase}: {error.request.url}"
    return str(error)


async def pipe_to_tar(
    response: httpx.Response,
    fixture: Fixture,
    progress: Progress,
    task_id: int,
) -> None:
    process = await asyncio.create_subprocess_exec(
        "tar",
        "xzf",
        "-",
        *(f"--exclude={pattern}" for pattern in fixture.excludes),
        "--strip-components=1",
        "-C",
        str(fixture.dest),
        stdin=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    assert process.stdin is not None
    assert process.stderr is not None

    try:
        downloaded = 0
        async for chunk in response.aiter_bytes(chunk_size=1024 * 1024):
            if not chunk:
                continue
            process.stdin.write(chunk)
            await process.stdin.drain()
            downloaded += len(chunk)
            progress.update(task_id, advance=len(chunk))

        process.stdin.close()
        await process.stdin.wait_closed()
        stderr = await process.stderr.read()
        return_code = await process.wait()
    except BaseException:
        if process.returncode is None:
            with suppress(ProcessLookupError):
                process.kill()
            await process.wait()
        raise

    if return_code != 0:
        message = stderr.decode(errors="replace").strip()
        if not message:
            message = f"tar exited with status {return_code}"
        raise RuntimeError(message)

    total = int(response.headers.get("content-length") or 0)
    if not total:
        progress.update(task_id, total=downloaded)


async def download_and_extract_stream(
    client: httpx.AsyncClient,
    fixture: Fixture,
    progress: Progress,
    task_id: int,
) -> None:
    fixture.dest.mkdir(parents=True, exist_ok=True)
    async with client.stream("GET", fixture.url) as response:
        response.raise_for_status()
        total = int(response.headers.get("content-length") or 0)
        if total:
            progress.update(task_id, total=total)

        await pipe_to_tar(response, fixture, progress, task_id)


async def download_and_extract(
    client: httpx.AsyncClient,
    fixture: Fixture,
    progress: Progress,
) -> None:
    if all(path.is_dir() for path in fixture.exists_paths):
        paths = ", ".join(str(path) for path in fixture.exists_paths)
        console.print(f"  Already exists: {paths}")
        return
    if fixture.dest.exists():
        raise RuntimeError(
            f"exists but does not contain the expected test fixtures: {fixture.dest} "
            "(remove or rename it and rerun)"
        )

    fixture.dest.mkdir(parents=True, exist_ok=True)
    task_id = progress.add_task(fixture.label, start=False, total=None)

    async def action() -> None:
        progress.reset(task_id, start=True, completed=0, total=None)
        await download_and_extract_stream(client, fixture, progress, task_id)
        progress.update(task_id, description=f"{fixture.label} done")

    await retry(fixture.label, action)


async def download_all(fixtures: list[Fixture], progress: Progress) -> None:
    timeout = httpx.Timeout(60.0)
    async with httpx.AsyncClient(follow_redirects=True, timeout=timeout) as client:
        tasks = [
            asyncio.create_task(download_and_extract(client, fixture, progress))
            for fixture in fixtures
        ]
        try:
            await asyncio.gather(*tasks)
        except Exception:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            raise


def task_status(task, label_width: int) -> str:
    completed = human_bytes(task.completed)
    if task.total is None:
        status = "done" if task.finished else ""
        return f"{task.description:<{label_width}} {completed:>10} {status:>6}"

    percent = 0 if task.total == 0 else task.completed / task.total * 100
    remaining = task.time_remaining
    if task.finished:
        suffix = "done"
    elif remaining is not None:
        suffix = f"eta {format_duration(remaining)}"
    else:
        suffix = ""
    return (
        f"{task.description:<{label_width}} "
        f"{completed:>10}/{human_bytes(task.total):<10} "
        f"({percent:>3.0f}%, {suffix:>8})"
    )


def format_duration(seconds: float) -> str:
    seconds = max(0, round(seconds))
    minutes, seconds = divmod(seconds, 60)
    hours, minutes = divmod(minutes, 60)
    if hours:
        return f"{hours}h{minutes:02d}m"
    if minutes:
        return f"{minutes}m{seconds:02d}s"
    return f"{seconds}s"


def timestamp() -> str:
    return datetime.now().strftime("%H:%M:%S")


async def report_progress(progress: Progress) -> None:
    try:
        while True:
            await asyncio.sleep(5)
            tasks = list(progress.tasks)
            if tasks:
                label_width = max(len(task.description) for task in progress.tasks)
                console.print(
                    f"  [{timestamp()}] "
                    + " | ".join(task_status(task, label_width) for task in tasks)
                )
    except asyncio.CancelledError:
        return


def count_json_files(path: Path) -> int:
    return sum(1 for _ in path.rglob("*.json"))


def print_summary() -> None:
    console.print("=== Done ===")
    console.print("Fixture directories:")
    for directory in [
        MAIN_STABLE_DIR / "state_tests",
        MAIN_STABLE_DIR / "blockchain_tests",
        MAIN_DEVELOP_DIR / "state_tests",
        MAIN_DEVELOP_DIR / "blockchain_tests",
        EEST_DIR / "state_tests",
        EEST_DIR / "blockchain_tests",
        LEGACY_DIR / "Cancun" / "GeneralStateTests",
        LEGACY_DIR / "Constantinople" / "GeneralStateTests",
    ]:
        if directory.is_dir():
            console.print(f"  {directory} ({count_json_files(directory)} JSON files)")


async def main() -> int:
    fixtures = fixture_plan()
    if not fixtures:
        console.print("No fixtures selected.")
        return 0

    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)
    console.print("=== Fetching EEST fixtures ===")

    progress = Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        BarColumn(),
        TaskProgressColumn(),
        DownloadStatusColumn(),
        TransferSpeedColumn(),
        TimeRemainingColumn(),
        console=console,
        transient=True,
    )

    error: Exception | None = None
    with progress:
        reporter = None
        if not console.is_terminal:
            reporter = asyncio.create_task(report_progress(progress))
        try:
            await download_all(fixtures, progress)
        except Exception as err:
            error = err
        finally:
            if reporter is not None:
                reporter.cancel()
                await reporter

    if error is not None:
        console.print(f"[red]error:[/red] {short_error(error)}")
        return 1

    print_summary()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(asyncio.run(main()))
    except KeyboardInterrupt:
        console.print("\n[red]cancelled[/red]")
        sys.exit(130)
