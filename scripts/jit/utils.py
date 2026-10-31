"""Shared utilities for evm2-jit analysis scripts."""

import functools
import os
import re
import shutil
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


def cargo_build(root: str, target_dir: str | None = None) -> str:
    """Build the CLI binary and return its path.

    Always builds non-incrementally: incremental builds split crates into more codegen
    units, which changes the host code and skews head-vs-base measurements.
    """
    target_dir = target_dir or _shared_target_dir()
    env = {**os.environ, **llvm_env(root), "CARGO_TARGET_DIR": target_dir}
    env["CARGO_INCREMENTAL"] = "0"
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "evm2-cli", "--bin", "evm2", "--all-features"],
        check=True,
        cwd=root,
        env=env,
    )
    return os.path.join(target_dir, "debug", "evm2")


def llvm_env(root: str) -> dict[str, str]:
    """Put the LLVM version that ``root`` requires first in PATH, if it is installed.

    Base and head can require different LLVM versions, and llvm-sys otherwise uses
    whichever llvm-config comes first in PATH.
    """
    with open(os.path.join(root, "crates", "jit", "llvm", "Cargo.toml")) as f:
        # Older code selects LLVM through an inkwell feature, newer code through llvm-sys.
        m = re.search(r'"llvm(\d+)-\d+"|^llvm-sys = "(\d+)\d"', f.read(), re.M)
    version = m and (m[1] or m[2])
    llvm_config = version and shutil.which(f"llvm-config-{version}")
    if not llvm_config:
        return {}
    bindir = subprocess.run(
        [llvm_config, "--bindir"], capture_output=True, text=True, check=True
    ).stdout.strip()
    return {"PATH": bindir + os.pathsep + os.environ.get("PATH", "")}


def get_benches(binary: str) -> dict[str, str]:
    """Map each built-in benchmark to its fixture path, as listed by ``run --list``."""
    r = subprocess.run(
        [binary, "run", "--list"],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    benches = {}
    for line in r.stdout.splitlines():
        name, _, path = line.strip().partition("\t")
        if name:
            benches[name] = path
    return benches


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


PERF_EVENTS = "instructions:u,cycles:u,task-clock"
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


def time_cli(binary: str, args: list[str], perf: bool) -> tuple[int | None, int | None, float]:
    """Run the CLI and return ``(instructions, cycles, seconds)``.

    With *perf*, returns user-space instructions, user-space cycles and task clock (CPU time)
    from ``perf stat``. Otherwise returns no counts and wall time.
    """
    cmd = [binary, *args]
    if not perf:
        start = time.perf_counter()
        subprocess.run(
            cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, check=True
        )
        return None, None, time.perf_counter() - start
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
    return int(stats["instructions:u"]), int(stats["cycles:u"]), stats["task-clock"] / 1e3


# Size of the strings the kernel puts at the top of a new process's stack: arguments, environment
# and the executable path.
STACK_STRINGS = 32768


@functools.cache
def no_aslr() -> list[str]:
    """Command prefix that disables address space randomization, empty where that is denied."""
    try:
        ok = (
            subprocess.run(["setarch", "-R", "true"], capture_output=True, check=False).returncode
            == 0
        )
    except FileNotFoundError:
        ok = False
    if not ok:
        eprint("warning: cannot disable address space randomization; runtime cycles will be noisy")
    return ["setarch", "-R"] if ok else []


def fixed_stack_env(binary: str, args: list[str], extra: dict[str, str]) -> dict[str, str]:
    """Return an environment that makes the process's stack strings ``STACK_STRINGS`` bytes.

    Where the stack starts depends on the length of the binary path, the arguments and the
    environment, and the starting offset changes cycles by several percent (4K aliasing, cache
    sets and the alignment of hot data). A fixed total puts the stack in the same place for
    every revision and bench size.
    """
    env = {**os.environ, **extra, "EVM2_BENCH_PAD": ""}
    strings = [binary, binary, *args, *(f"{k}={v}" for k, v in env.items())]
    used = sum(len(os.fsencode(s)) + 1 for s in strings)
    env["EVM2_BENCH_PAD"] = "x" * max(STACK_STRINGS - used, 0)
    return env


class NoBenchCounts(Exception):
    """The binary ran without counting a benchmark loop: it predates ``EVM2_PERF_CONTROL``."""


def perf_counts(binary: str, args: list[str]) -> tuple[int, int, int | None]:
    """Run ``evm2 run`` under ``perf stat``; return ``(instructions, cycles, iterations)``.

    The counts cover its bench loop, and ``iterations`` is the number of iterations the CLI
    reports having counted on a pipe of its own (``EVM2_PERF_REPORT``), or ``None`` when the
    binary predates that report. The pipe carries nothing a benchmark name can write.

    The counters start disabled and the CLI enables them around the loop through
    ``EVM2_PERF_CONTROL``, so the compile cost, which is large and noisy, is not counted.

    Runs without address space randomization and with a fixed stack layout, so the same binary
    gives the same counts from run to run and two revisions are compared on equal terms.
    """
    ctl_read, ctl_write = os.pipe()
    ack_read, ack_write = os.pipe()
    report_read, report_write = os.pipe()
    fds = [ctl_read, ctl_write, ack_read, ack_write, report_read, report_write]
    try:
        with tempfile.NamedTemporaryFile("r") as out:
            subprocess.run(
                [
                    *no_aslr(),
                    "perf",
                    "stat",
                    "-x,",
                    "-e",
                    RUNTIME_EVENTS,
                    "-o",
                    out.name,
                    "--control",
                    f"fd:{ctl_read},{ack_write}",
                    "-D",
                    "-1",
                    binary,
                    *args,
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
                env=fixed_stack_env(
                    binary,
                    args,
                    {
                        "EVM2_PERF_CONTROL": f"{ctl_write},{ack_read}",
                        "EVM2_PERF_REPORT": str(report_write),
                    },
                ),
                pass_fds=fds,
            )
            # The run is over, so the report is in the pipe once the write end closes.
            os.close(report_write)
            fds.remove(report_write)
            with os.fdopen(report_read, closefd=False) as report:
                counted = report.read().split()
            stats = {}
            for line in out:
                parts = line.strip().split(",")
                if len(parts) > 2:
                    try:
                        stats[parts[2]] = float(parts[0])
                    except ValueError:
                        pass
    finally:
        for fd in fds:
            os.close(fd)
    if not stats.get("instructions:u"):
        raise NoBenchCounts(binary)
    return int(stats["instructions:u"]), int(stats["cycles:u"]), sum(map(int, counted)) or None
