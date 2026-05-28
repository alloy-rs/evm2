#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.13"
# ///
"""Generate an evm2 coverage report from the differential fuzzer.

The report intentionally excludes the fuzzer crate itself so the resulting
coverage reflects evm2 and other runtime code exercised by generated cases.

Examples:
    ./scripts/fuzzer_coverage.py --duration 3m -j 0 --open
    ./scripts/fuzzer_coverage.py --cases 100000 --backend packed
    ./scripts/fuzzer_coverage.py --all-backends --duration 1m -j 0
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

from utils import cargo_env, repo_root

ROOT = Path(repo_root())
DEFAULT_OUTPUT_DIR = ROOT / "target" / "llvm-cov" / "fuzzer-html"
DEFAULT_IGNORE_REGEX = r"(^|/)crates/fuzzer/"
BACKENDS = ("tco", "packed", "single_return", "unpacked")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    limit = parser.add_mutually_exclusive_group()
    limit.add_argument("--duration", default="3m", help="Fuzz duration per backend, e.g. 30s, 3m, 1h")
    limit.add_argument("--cases", type=int, help="Number of generated cases per backend")
    parser.add_argument("--seed", type=int, help="Seed to pass to the fuzzer. Random if omitted")
    parser.add_argument(
        "-j",
        "--threads",
        "--jobs",
        type=int,
        default=0,
        help="Fuzzer worker threads. Zero uses logical cores",
    )
    parser.add_argument(
        "--backend",
        action="append",
        choices=BACKENDS,
        help="Dispatch backend to cover. May be passed more than once. Defaults to packed",
    )
    parser.add_argument(
        "--all-backends",
        action="store_true",
        help="Run coverage with all explicit dispatch backends",
    )
    parser.add_argument(
        "--toolchain",
        default="nightly",
        help="Rust toolchain for cargo llvm-cov. Defaults to nightly so tco works",
    )
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--ignore-filename-regex", default=DEFAULT_IGNORE_REGEX)
    parser.add_argument("--no-clean", action="store_true", help="Do not clean previous llvm-cov data first")
    parser.add_argument("--open", action="store_true", help="Open the generated HTML report")
    parser.add_argument(
        "fuzzer_args",
        nargs=argparse.REMAINDER,
        help="Additional arguments passed to evm2-fuzzer after --",
    )
    args = parser.parse_args()
    if args.fuzzer_args and args.fuzzer_args[0] == "--":
        args.fuzzer_args = args.fuzzer_args[1:]
    return args


def cargo(toolchain: str) -> list[str]:
    return ["cargo", f"+{toolchain}"] if toolchain else ["cargo"]


def run(command: list[str], *, env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def fuzzer_limit_args(args: argparse.Namespace) -> list[str]:
    if args.cases is not None:
        return ["--cases", str(args.cases)]
    return ["--duration", args.duration]


def main() -> int:
    args = parse_args()
    backends = list(BACKENDS if args.all_backends else (args.backend or ["packed"]))
    cargo_cmd = cargo(args.toolchain)

    print(f"backends: {', '.join(backends)}")
    print(f"excluding files matching: {args.ignore_filename_regex}")

    if not args.no_clean:
        run([*cargo_cmd, "llvm-cov", "clean", "--workspace"])

    base_fuzzer_args = [
        *([] if args.seed is None else ["--seed", str(args.seed)]),
        *fuzzer_limit_args(args),
        "-j",
        str(args.threads),
        *args.fuzzer_args,
    ]

    for backend in backends:
        env = {**cargo_env(), "EVM2_DISPATCH_BACKEND": backend}
        run(
            [
                *cargo_cmd,
                "llvm-cov",
                "run",
                "-p",
                "evm2-fuzzer",
                "--no-report",
                "--",
                *base_fuzzer_args,
            ],
            env=env,
        )

    report = [
        *cargo_cmd,
        "llvm-cov",
        "report",
        "--html",
        "--output-dir",
        str(args.output_dir),
        "--ignore-filename-regex",
        args.ignore_filename_regex,
    ]
    if args.open:
        report.append("--open")
    run(report)
    print(f"coverage report: {args.output_dir}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as err:
        raise SystemExit(err.returncode) from err
    except KeyboardInterrupt:
        raise SystemExit(130)
