"""Shared utilities for evm2-jit analysis scripts."""

import os
import re
import subprocess
import sys
import tempfile
import time

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def eprint(*args, **kwargs):
    print(*args, file=sys.stderr, **kwargs)


def strip_ansi(s: str) -> str:
    return ANSI_RE.sub("", s)


def repo_root() -> str:
    return os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def _shared_target_dir() -> str:
    """Return a shared CARGO_TARGET_DIR so worktrees reuse the same build cache."""
    return os.path.join(repo_root(), "target")


def cargo_env(rust_log: str | None = None) -> dict[str, str]:
    env = {**os.environ, "NO_COLOR": "1", "CARGO_TARGET_DIR": _shared_target_dir()}
    if rust_log:
        env["RUST_LOG"] = rust_log
    return env


def cargo_build(root: str, incremental: bool = True, target_dir: str | None = None) -> str:
    """Build the CLI binary and return its path."""
    target_dir = target_dir or _shared_target_dir()
    env = {**os.environ, "CARGO_TARGET_DIR": target_dir}
    if not incremental:
        env["CARGO_INCREMENTAL"] = "0"
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "evm2-cli", "--bin", "evm2", "--all-features"],
        check=True,
        cwd=root,
        env=env,
    )
    return os.path.join(target_dir, "debug", "evm2")


def get_benches(binary: str) -> list[str]:
    r = subprocess.run(
        [binary, "run", "--list"],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    return [line.strip() for line in r.stdout.splitlines() if line.strip()]


def run_cli(
    binary: str,
    args: list[str],
    env: dict[str, str] | None = None,
    capture_stderr: bool = False,
) -> str:
    """Run the CLI binary and return its stdout.

    stderr is printed to the terminal.  When *capture_stderr* is True it is
    also appended to the returned string (needed for trace-log analyses).
    """
    r = subprocess.run(
        [binary, *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    if r.stderr:
        sys.stderr.write(r.stderr)
    if r.returncode != 0:
        raise subprocess.CalledProcessError(
            r.returncode,
            r.args,
            output=r.stdout,
            stderr=r.stderr,
        )
    if capture_stderr:
        return r.stdout + r.stderr
    return r.stdout


PERF_EVENTS = "instructions:u,task-clock"
RUNTIME_EVENTS = "instructions:u,cycles:u"


def perf_available() -> bool:
    """Whether ``perf stat`` can count user-space instructions."""
    try:
        r = subprocess.run(
            ["perf", "stat", "-x,", "-e", PERF_EVENTS, "true"],
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        return False
    return r.returncode == 0 and "<not" not in r.stderr and "instructions" in r.stderr


def time_cli(binary: str, args: list[str], perf: bool) -> tuple[int | None, float]:
    """Run the CLI and return ``(instructions, seconds)``.

    With *perf*, returns user-space instructions and task clock (CPU time) from
    ``perf stat``. Otherwise returns no instruction count and wall time.
    """
    cmd = [binary, *args]
    if not perf:
        start = time.perf_counter()
        subprocess.run(
            cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, check=True
        )
        return None, time.perf_counter() - start
    with tempfile.NamedTemporaryFile("r") as out:
        subprocess.run(
            ["perf", "stat", "-x,", "-e", PERF_EVENTS, "-o", out.name, *cmd],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            check=True,
        )
        stats = {}
        for line in out:
            parts = line.strip().split(",")
            if len(parts) > 2:
                try:
                    stats[parts[2]] = float(parts[0])
                except ValueError:
                    pass
    return int(stats["instructions:u"]), stats["task-clock"] / 1e3


def perf_counts(binary: str, args: list[str]) -> tuple[int, int]:
    """Run the CLI under ``perf stat`` and return user-space ``(instructions, cycles)``."""
    with tempfile.NamedTemporaryFile("r") as out:
        subprocess.run(
            ["perf", "stat", "-x,", "-e", RUNTIME_EVENTS, "-o", out.name, binary, *args],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            check=True,
        )
        stats = {}
        for line in out:
            parts = line.strip().split(",")
            if len(parts) > 2:
                try:
                    stats[parts[2]] = float(parts[0])
                except ValueError:
                    pass
    return int(stats["instructions:u"]), int(stats["cycles:u"])
