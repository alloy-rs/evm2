#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.13"
# dependencies = ["tabulate>=0.9", "tqdm>=4.67.3"]
# ///
"""Unified benchmark tool for evm2-jit.

Collects codegen stats, compile times, jump resolution, and constant-input
statistics across benchmarks. Supports diffing against a base git revision.

Examples:
    # Codegen + compile time diff against main (default):
    ./scripts/jit/bench.py /tmp/bench --diff main

    # All analyses on current branch only:
    ./scripts/jit/bench.py /tmp/bench --codegen-lines --compile-times --jump-resolution --input-stats

    # Only jump resolution for specific benchmarks:
    ./scripts/jit/bench.py /tmp/bench --jump-resolution seaport usdc_proxy

    # Include mainnet .bin files:
    ./scripts/jit/bench.py /tmp/bench --diff main --extra-dir tmp/mainnet

    # Only compile times:
    ./scripts/jit/bench.py /tmp/bench --diff main --compile-times

    # Only codegen line counts:
    ./scripts/jit/bench.py /tmp/bench --diff main --codegen-lines

    # More compile-cost samples:
    ./scripts/jit/bench.py /tmp/bench --diff main --runs 5
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import statistics
import subprocess
import sys
from abc import ABC, abstractmethod
from dataclasses import dataclass, field

from tabulate import tabulate
from tqdm import tqdm
from utils import (
    cargo_build,
    cargo_env,
    eprint,
    get_benches,
    perf_available,
    perf_counts,
    repo_root,
    run_cli,
    strip_ansi,
    time_cli,
)

# ---------------------------------------------------------------------------
# Bench discovery
# ---------------------------------------------------------------------------


def find_extra_benches(extra_dirs: list[str], root: str) -> list[str]:
    """Find benchmarks in extra directories.

    For directories containing ``bytecode.bin``, returns the directory path —
    the CLI accepts those directly and uses the directory name as the bench
    name. For bare ``.bin`` files, returns the absolute path.
    """
    paths = []
    for d in extra_dirs:
        d = os.path.join(root, d) if not os.path.isabs(d) else d
        if not os.path.isdir(d):
            eprint(f"warning: extra dir {d!r} does not exist, skipping")
            continue
        for entry in sorted(os.listdir(d)):
            entry_path = os.path.join(d, entry)
            # Directory containing bytecode.bin — pass the directory.
            if os.path.isdir(entry_path) and os.path.isfile(
                os.path.join(entry_path, "bytecode.bin")
            ):
                paths.append(os.path.abspath(entry_path))
            # Direct .bin file.
            elif os.path.isfile(entry_path) and entry.endswith(".bin"):
                paths.append(os.path.abspath(entry_path))
    return paths


def bench_name(bench: str) -> str:
    """Short display name for a benchmark."""
    if os.path.isabs(bench) or os.sep in bench:
        parent = os.path.basename(os.path.dirname(bench))
        stem = os.path.splitext(os.path.basename(bench))[0]
        name = parent if parent != "" and stem == "bytecode" else stem
        if len(name) > 16 and name.startswith("0x"):
            return name[:12] + "…"
        return name
    return bench


def dump_subdir(bench: str) -> str:
    """Expected subdirectory name in the dump directory for a benchmark."""
    if os.path.isabs(bench) or os.sep in bench:
        parent = os.path.basename(os.path.dirname(bench))
        stem = os.path.splitext(os.path.basename(bench))[0]
        return parent if parent != "" and stem == "bytecode" else stem
    return bench


# ---------------------------------------------------------------------------
# Formatting helpers
# ---------------------------------------------------------------------------


def _indicator(delta: float) -> str:
    """🟢 for improvement (negative delta), 🔴 for regression, empty for neutral."""
    if delta < 0:
        return " 🟢"
    if delta > 0:
        return " 🔴"
    return ""


def fmt_diff(base: int, current: int) -> str:
    d = current - base
    return "=" if d == 0 else f"{d:+d}"


def fmt_rel(pct: float) -> str:
    if round(pct, 1) == 0:
        return "="
    return f"{pct:+.1f}%{_indicator(pct)}"


def fmt_pct(base: float | int, current: float | int) -> str:
    if base == 0:
        return "-"
    return fmt_rel((current - base) / base * 100)


def fmt_geomean(pairs: list[tuple[float, float]]) -> str:
    """Geometric mean of the current/base ratios, so every benchmark weighs the same."""
    ratios = [c / b for b, c in pairs if b > 0 and c > 0]
    if not ratios:
        return "-"
    return fmt_rel((statistics.geometric_mean(ratios) - 1) * 100)


def fmt_detail(current: str | int, base: str | int, diff: str) -> str:
    return f"{base}<br>{current}<br>{diff}"


def fmt_detail_bold(current: str | int, base: str | int, diff: str) -> str:
    return f"**{fmt_detail(current, base, diff)}**"


def is_noise(base: float | int, current: float | int, noise: float) -> bool:
    """Whether the relative change is within ``noise`` percent (or unmeasurable)."""
    if base == 0:
        return current == 0
    pct = (current - base) / base * 100
    return abs(pct) <= noise


# Noise thresholds for skipping rows in the always-visible summary tables.
# Rows where every change is within the threshold are dropped from the summary
# (the `<details>` table still shows them).
NOISE_CODEGEN = 1.0  # codegen line counts, jit size, spills, reloads
NOISE_INSTRUCTIONS = 1.0  # retired instructions (~0.3% run-to-run variance)
NOISE_TIME = 5.0  # compile times (high run-to-run variance)


def fmt_dur(s: float) -> str:
    if s == 0:
        return "-"
    if s < 0.001:
        return f"{s * 1e6:.1f}µs"
    if s < 0.01:
        return f"{s * 1e3:.2f}ms"
    if s < 1.0:
        return f"{s * 1e3:.1f}ms"
    return f"{s:.3f}s"


def fmt_count(n: float) -> str:
    if n == 0:
        return "-"
    for div, suffix in ((1e9, "G"), (1e6, "M"), (1e3, "K")):
        if n >= div:
            return f"{n / div:.2f}{suffix}"
    return f"{n:.0f}"


def fmt_size(b: int) -> str:
    if b == 0:
        return "-"
    if b < 1024:
        return f"{b} B"
    if b < 1024 * 1024:
        return f"{b / 1024:.1f} KiB"
    return f"{b / 1024 / 1024:.1f} MiB"


def line_count(path: str) -> int:
    try:
        with open(path) as f:
            return sum(1 for _ in f)
    except FileNotFoundError:
        return 0


STRING_OFFSET_RE = re.compile(r"\s*# string offset=\d+")


def normalize_dump(text: str, dump_dir: str) -> str:
    """Remove the dump path embedded in debug info, and the string offsets that depend on it."""
    return STRING_OFFSET_RE.sub("", text.replace(dump_dir, "<dump>"))


def asm_digest(dump_dir: str, bench: str) -> str | None:
    """Hash a benchmark's ``opt.s``, ignoring the dump path embedded in its debug info."""
    try:
        with open(os.path.join(dump_dir, dump_subdir(bench), "opt.s")) as f:
            text = f.read()
    except FileNotFoundError:
        return None
    return hashlib.sha256(normalize_dump(text, dump_dir).encode()).hexdigest()


def spill_reload_counts(path: str) -> tuple[int, int]:
    """Count spill and reload annotations in an assembly file."""
    spills = 0
    reloads = 0
    try:
        with open(path) as f:
            for line in f:
                if "Spill" in line:
                    spills += 1
                if "Reload" in line:
                    reloads += 1
    except FileNotFoundError:
        pass
    return spills, reloads


def i256_load_store_counts(path: str) -> tuple[int, int]:
    """Count 'load i256' and 'store i256' instructions in an LLVM IR file."""
    loads = 0
    stores = 0
    try:
        with open(path) as f:
            for line in f:
                if "load i256" in line:
                    loads += 1
                if "store i256" in line:
                    stores += 1
    except FileNotFoundError:
        pass
    return loads, stores


def mresize_call_count(path: str) -> int:
    """Count mresize builtin calls in an LLVM IR file."""
    calls = 0
    try:
        with open(path) as f:
            for line in f:
                if "call" in line and "@__revmc_builtin_mresize" in line:
                    calls += 1
    except FileNotFoundError:
        pass
    return calls


DURATION_RE = re.compile(r"(\d+(?:\.\d+)?)\s*(ns|µs|us|ms|s)")
DURATION_UNITS = {"ns": 1e-9, "µs": 1e-6, "us": 1e-6, "ms": 1e-3, "s": 1.0}


def parse_duration(s: str) -> float:
    """Parse a Rust-style Duration debug string to seconds."""
    m = DURATION_RE.search(s)
    if not m:
        return 0.0
    return float(m.group(1)) * DURATION_UNITS[m.group(2)]


class _Tee:
    """Write to multiple streams simultaneously (e.g. stdout + a file)."""

    def __init__(self, *streams):
        self._streams = streams

    def write(self, s):
        for st in self._streams:
            st.write(s)

    def flush(self):
        for st in self._streams:
            st.flush()

    def isatty(self):
        return False


def print_table(headers: list[str], rows: list[list], right_cols: int | None = None):
    """Print a markdown table. All columns except the first are right-aligned by default."""
    n = len(headers)
    if right_cols is None:
        right_cols = n - 1
    align = ("left",) + ("right",) * right_cols + ("left",) * (n - 1 - right_cols)
    print(tabulate(rows, headers=headers, tablefmt="pipe", colalign=align))
    print()


def parse_remarks(dump_dir: str, bench: str) -> dict[str, float]:
    """Parse remarks.txt for a benchmark, returning timing fields in seconds."""
    path = os.path.join(dump_dir, dump_subdir(bench), "remarks.txt")
    result = {}
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if line.startswith("Generated files"):
                    break
                parts = line.split(":", 1)
                if len(parts) == 2:
                    key = parts[0].strip().lstrip("- ")
                    dur = parse_duration(parts[1])
                    if dur > 0:
                        result[key] = dur
    except FileNotFoundError:
        pass
    return result


JIT_SIZE_RE = re.compile(r"(?:\(|:\s)(\d+)\s*B\)?")


def parse_jit_size(dump_dir: str, bench: str) -> int:
    """Parse JIT code size in bytes from remarks.txt."""
    path = os.path.join(dump_dir, dump_subdir(bench), "remarks.txt")
    in_section = False
    try:
        with open(path) as f:
            for line in f:
                if line.startswith("JIT code sizes"):
                    in_section = True
                    continue
                if in_section:
                    m = JIT_SIZE_RE.search(line)
                    if m:
                        return int(m.group(1))
    except FileNotFoundError:
        pass
    return 0


# ---------------------------------------------------------------------------
# Analysis classes
# ---------------------------------------------------------------------------


@dataclass
class Side:
    """Collected results for one revision."""

    label: str
    rev: str
    binary: str
    dump_dir: str
    # Source tree the binary was built from.
    root: str
    outputs: dict[str, str] = field(default_factory=dict)
    failures: dict[str, str] = field(default_factory=dict)
    # Compile-cost samples per bench: (instructions | None, cycles | None, seconds).
    samples: dict[str, list[tuple[int | None, int | None, float]]] = field(default_factory=dict)
    # Execution instructions and cycles per iteration, per bench.
    runtime: dict[str, float] = field(default_factory=dict)
    runtime_cycles: dict[str, float] = field(default_factory=dict)


class Analysis(ABC):
    """Base class for benchmark analyses."""

    @abstractmethod
    def needs_codegen(self) -> bool:
        """Whether this analysis needs full compilation with -o dump_dir."""
        ...

    @abstractmethod
    def rust_log(self) -> str | None:
        """RUST_LOG value needed, or None."""
        ...

    def needs_timing(self) -> bool:
        """Whether this analysis needs compile-cost samples."""
        return False

    def needs_runtime(self) -> bool:
        """Whether this analysis needs execution instruction counts."""
        return False

    @abstractmethod
    def report(self, benches: list[str], side: Side):
        """Print results for a single revision."""
        ...

    def report_diff(self, benches: list[str], cur: Side, base: Side):
        """Print diff results. Default falls back to solo report."""
        self.report(benches, cur)


def column_totals(rows: dict[str, dict], columns: list[str]) -> dict:
    return {c: sum(r[c] for r in rows.values()) for c in columns}


def print_solo(rows: dict[str, dict], columns: list[str], fmt):
    """Print every row and a TOTAL row. ``fmt(column, value)`` formats a cell."""
    totals = column_totals(rows, columns)
    table = [[name, *(fmt(c, r[c]) for c in columns)] for name, r in rows.items()]
    table.append(["**TOTAL**", *(f"**{fmt(c, totals[c])}**" for c in columns)])
    print_table(["benchmark", *columns], table)


def print_diff(
    cur_rows: dict[str, dict],
    base_rows: dict[str, dict],
    columns: list[str],
    fmt,
    changed,
    details: str,
    summary_columns: list[str] | None = None,
):
    """Print a summary of changed rows with TOTAL and GEOMEAN, then every value in ``<details>``.

    ``changed`` gets ``{column: (base, current)}`` and decides whether a row goes in the summary.
    """
    summary_columns = summary_columns or columns
    cur_totals = column_totals(cur_rows, columns)
    base_totals = column_totals(base_rows, columns)
    empty = dict.fromkeys(columns, 0)
    pairs = {
        name: {c: (base_rows.get(name, empty)[c], r[c]) for c in columns}
        for name, r in cur_rows.items()
    }

    table = [
        [name, *(fmt_pct(*ps[c]) for c in summary_columns)]
        for name, ps in pairs.items()
        if changed(ps)
    ]
    table.append(
        [
            "**TOTAL**",
            *(f"**{fmt_pct(base_totals[c], cur_totals[c])}**" for c in summary_columns),
        ]
    )
    table.append(
        [
            "**GEOMEAN**",
            *(f"**{fmt_geomean([ps[c] for ps in pairs.values()])}**" for c in summary_columns),
        ]
    )
    print_table(["benchmark", *summary_columns], table)

    def cell(c, b, v, detail=fmt_detail):
        return detail(fmt(c, v), fmt(c, b), fmt_pct(b, v))

    print(f"<details><summary>{details}</summary>\n")
    detail_table = [[name, *(cell(c, *ps[c]) for c in columns)] for name, ps in pairs.items()]
    detail_table.append(
        [
            "**TOTAL**",
            *(cell(c, base_totals[c], cur_totals[c], fmt_detail_bold) for c in columns),
        ]
    )
    print_table(["benchmark", *columns], detail_table)
    print("</details>\n")


CODEGEN_FILES = ["unopt.ll", "opt.ll", "opt.s"]
CODEGEN_COLUMNS = [
    *CODEGEN_FILES,
    "jit size",
    "i256 loads",
    "i256 stores",
    "mresize",
    "spills",
    "reloads",
]


def fmt_codegen(col: str, v: int) -> str | int:
    return fmt_size(v) if col == "jit size" else v


class CodegenLines(Analysis):
    def needs_codegen(self) -> bool:
        return True

    def rust_log(self) -> str | None:
        return None

    def _collect(self, benches, dump_dir) -> dict[str, dict[str, int]]:
        """Collect line counts per file type + JIT size + i256 loads/stores + spills/reloads."""
        rows = {}
        for bench in benches:
            sub = os.path.join(dump_dir, dump_subdir(bench))
            counts = [line_count(os.path.join(sub, f)) for f in CODEGEN_FILES]
            if all(c == 0 for c in counts):
                continue
            opt_ll = os.path.join(sub, "opt.ll")
            i256_loads, i256_stores = i256_load_store_counts(opt_ll)
            spills, reloads = spill_reload_counts(os.path.join(sub, "opt.s"))
            values = [
                *counts,
                parse_jit_size(dump_dir, bench),
                i256_loads,
                i256_stores,
                mresize_call_count(opt_ll),
                spills,
                reloads,
            ]
            rows[bench_name(bench)] = dict(zip(CODEGEN_COLUMNS, values))
        return rows

    def report(self, benches, side):
        print("### Codegen statistics\n")
        print_solo(self._collect(benches, side.dump_dir), CODEGEN_COLUMNS, fmt_codegen)

    def report_diff(self, benches, cur, base):
        print("### Codegen statistics\n")
        print(opt_s_identity(benches, cur, base))
        print_diff(
            self._collect(benches, cur.dump_dir),
            self._collect(benches, base.dump_dir),
            CODEGEN_COLUMNS,
            fmt_codegen,
            all_noise(NOISE_CODEGEN),
            "Full details",
        )


def opt_s_identity(benches: list[str], cur: Side, base: Side) -> str:
    """Summarize how many benchmarks produced identical `opt.s` on both sides."""
    compared = changed = 0
    for bench in benches:
        c = asm_digest(cur.dump_dir, bench)
        b = asm_digest(base.dump_dir, bench)
        if c is None or b is None:
            continue
        compared += 1
        changed += c != b
    return f"`opt.s` changed in {changed}/{compared} benchmarks.\n"


PHASES = ["parse", "translate", "optimize", "codegen"]
SUMMARY_TIME_KEYS = ["instructions", "cycles", "time"]
TIME_KEYS = [*SUMMARY_TIME_KEYS, *PHASES]


def fmt_time_key(key: str, v: float) -> str:
    return fmt_count(v) if key in ("instructions", "cycles") else fmt_dur(v)


def time_changed(ps: dict[str, tuple[float, float]]) -> bool:
    # Instruction counts are stable enough to pick rows; time is the fallback.
    b, c = ps["instructions"]
    if b and c:
        return not is_noise(b, c, NOISE_INSTRUCTIONS)
    return not is_noise(*ps["time"], NOISE_TIME)


class CompileTime(Analysis):
    """Compile cost from ``evm2 run <bench> 0`` samples plus per-phase times from remarks.

    ``instructions``, ``cycles`` and ``time`` are medians over runs without dumps. They
    cover the whole process: startup, fixture loading, and compiling every contract in the fixture.
    Phase times come from the single dump run of the entry contract, so they are noisy and
    only appear in the details table; ``finalize`` is left out because it includes writing
    the IR and assembly dumps.
    """

    def needs_codegen(self) -> bool:
        return True

    def rust_log(self) -> str | None:
        return None

    def needs_timing(self) -> bool:
        return True

    def _collect(self, benches, side) -> dict[str, dict[str, float]]:
        rows = {}
        for bench in benches:
            samples = side.samples.get(bench)
            if not samples:
                continue
            remarks = parse_remarks(side.dump_dir, bench)
            instructions = [i for i, _, _ in samples if i is not None]
            cycles = [c for _, c, _ in samples if c is not None]
            rows[bench_name(bench)] = {
                "instructions": statistics.median(instructions) if instructions else 0,
                "cycles": statistics.median(cycles) if cycles else 0,
                "time": statistics.median(t for _, _, t in samples),
                **{p: remarks.get(p, 0.0) for p in PHASES},
            }
        return rows

    def report(self, benches, side):
        print("### Compile times\n")
        print_solo(self._collect(benches, side), TIME_KEYS, fmt_time_key)

    def report_diff(self, benches, cur, base):
        print("### Compile times\n")
        print_diff(
            self._collect(benches, cur),
            self._collect(benches, base),
            TIME_KEYS,
            fmt_time_key,
            time_changed,
            "Full compile times",
            SUMMARY_TIME_KEYS,
        )


RUNTIME_KEY = "instructions/iter"
RUNTIME_CYCLES_KEY = "cycles/iter"
RUNTIME_KEYS = [RUNTIME_KEY, RUNTIME_CYCLES_KEY]
# Cycles depend on caches, frequency and other load, unlike instruction counts.
NOISE_CYCLES = 5.0


def fmt_runtime_key(_key: str, v: float) -> str:
    return fmt_count(v)


def all_noise(noise: float):
    """Row filter for `print_diff`: keep rows where any column moved by more than `noise`%."""
    return lambda ps: not all(is_noise(b, c, noise) for b, c in ps.values())


def runtime_changed(ps: dict[str, tuple[float, float]]) -> bool:
    return not is_noise(*ps[RUNTIME_KEY], NOISE_INSTRUCTIONS) or not is_noise(
        *ps[RUNTIME_CYCLES_KEY], NOISE_CYCLES
    )


class Runtime(Analysis):
    """User-space instructions and cycles per JIT execution of each benchmark.

    Instruction counts are deterministic, like CodSpeed's simulation mode, so they work on a
    busy machine. Cycles also reflect cache misses and branch mispredictions, but vary with the
    machine and its load.
    """

    def needs_codegen(self) -> bool:
        return False

    def rust_log(self) -> str | None:
        return None

    def needs_runtime(self) -> bool:
        return True

    def _collect(self, benches, side) -> dict[str, dict[str, float]]:
        return {
            bench_name(b): {
                RUNTIME_KEY: side.runtime[b],
                RUNTIME_CYCLES_KEY: side.runtime_cycles.get(b, 0),
            }
            for b in benches
            if b in side.runtime
        }

    def report(self, benches, side):
        print("### Runtime\n")
        print_solo(self._collect(benches, side), RUNTIME_KEYS, fmt_runtime_key)

    def report_diff(self, benches, cur, base):
        print("### Runtime\n")
        print_diff(
            self._collect(benches, cur),
            self._collect(benches, base),
            RUNTIME_KEYS,
            fmt_runtime_key,
            runtime_changed,
            "Full runtime",
        )


# ---------------------------------------------------------------------------
# Jump resolution analysis
# ---------------------------------------------------------------------------


class JumpResolution(Analysis):
    def needs_codegen(self) -> bool:
        return False

    def rust_log(self) -> str | None:
        return "evm2_jit_codegen::bytecode=trace"

    @staticmethod
    def _last_match(pattern: str, text: str) -> int:
        matches = re.findall(pattern, text)
        return int(matches[-1]) if matches else 0

    @staticmethod
    def _count_matches(pattern: str, text: str) -> int:
        return len(re.findall(pattern, text))

    @classmethod
    def _analyze(cls, output: str) -> dict[str, int]:
        local_res = cls._last_match(r"local_jumps.*newly_resolved=(\d+)", output)
        non_adj = cls._count_matches(r"resolved non-adjacent jump", output)
        fixpt_res = cls._last_match(r"ba:.*newly_resolved=(\d+)", output)

        ba_unres_matches = re.findall(
            r"analyze:ba:.*unresolved dynamic jumps remain n=(\d+)", output
        )
        ba_ran = "analyze:ba:" in output

        if ba_unres_matches:
            unresolved = int(ba_unres_matches[-1])
        elif ba_ran:
            unresolved = 0
        else:
            unresolved = cls._last_match(
                r"local_jumps:.*unresolved dynamic jumps remain n=(\d+)", output
            )

        total = local_res + fixpt_res + unresolved
        return {
            "local": local_res,
            "non_adj": non_adj,
            "fixpt": fixpt_res,
            "unresolved": unresolved,
            "total": total,
        }

    def report(self, benches, side):
        print("### Jump resolution\n")
        headers = ["benchmark", "local", "nonadj", "fixpt", "unres", "total"]
        table = []
        for bench in benches:
            s = self._analyze(side.outputs.get(bench, ""))
            table.append(
                [
                    bench_name(bench),
                    s["local"],
                    s["non_adj"],
                    s["fixpt"],
                    s["unresolved"],
                    s["total"],
                ]
            )
        print_table(headers, table)


# ---------------------------------------------------------------------------
# IR stats analysis
# ---------------------------------------------------------------------------

IR_STAT_KEYS = [
    "total_insts",
    "live",
    "dead",
    "noops",
    "blocks",
    "block_min",
    "block_max",
    "block_avg",
    "block_median",
]


class BlockStats(Analysis):
    def needs_codegen(self) -> bool:
        return False

    def rust_log(self) -> str | None:
        return "evm2_jit_codegen::bytecode=trace"

    @staticmethod
    def _parse(output: str) -> dict[str, float] | None:
        m = re.search(
            r"ir stats"
            r" total_insts=(\d+)"
            r" live=(\d+)"
            r" dead=(\d+)"
            r" noops=(\d+)"
            r" blocks=(\d+)"
            r" block_min=(\d+)"
            r" block_max=(\d+)"
            r" block_avg=([\d.]+)"
            r" block_median=(\d+)",
            output,
        )
        if not m:
            return None
        return {
            "total_insts": int(m.group(1)),
            "live": int(m.group(2)),
            "dead": int(m.group(3)),
            "noops": int(m.group(4)),
            "blocks": int(m.group(5)),
            "block_min": int(m.group(6)),
            "block_max": int(m.group(7)),
            "block_avg": float(m.group(8)),
            "block_median": int(m.group(9)),
        }

    def report(self, benches, side):
        print("### IR stats\n")
        headers = ["benchmark"] + IR_STAT_KEYS
        table = []
        for bench in benches:
            s = self._parse(side.outputs.get(bench, ""))
            if not s:
                continue
            table.append([bench_name(bench)] + [s[k] for k in IR_STAT_KEYS])
        print_table(headers, table)

    def report_diff(self, benches, cur_side, base_side):
        print("### IR stats\n")
        headers = ["benchmark"] + IR_STAT_KEYS
        table = []
        for bench in benches:
            cur = self._parse(cur_side.outputs.get(bench, ""))
            base = self._parse(base_side.outputs.get(bench, ""))
            if not cur:
                continue
            if not base:
                table.append([bench_name(bench)] + [cur[k] for k in IR_STAT_KEYS])
            else:
                table.append([bench_name(bench)] + [fmt_pct(base[k], cur[k]) for k in IR_STAT_KEYS])
        print_table(headers, table)


# ---------------------------------------------------------------------------
# Constant-input stats analysis
# ---------------------------------------------------------------------------


class InputStats(Analysis):
    def needs_codegen(self) -> bool:
        return False

    def rust_log(self) -> str | None:
        return "evm2_jit_codegen::bytecode=trace"

    @staticmethod
    def _pct(a, b):
        return f"{a / b * 100:.0f}%" if b else "0%"

    @staticmethod
    def _parse(output: str):
        """Parse const input stats from trace output.

        Returns dict: opcode_name -> {opbyte, inputs, total, all_const, const_output, per_input}.
        """
        stats = {}
        current = None
        in_block = False

        for raw_line in output.splitlines():
            line = strip_ansi(raw_line)

            if "const input stats:" in line:
                in_block = True
                continue
            if not in_block:
                continue

            m = re.match(
                r"\s{2}(\S+)\s+(0x[0-9a-f]{2}), (\d+) occ, (\d+) inputs, all_const=(\d+)/(\d+)"
                r"(?:.*?const_output=(\d+)/(\d+))?",
                line,
            )
            if m:
                name = m.group(1)
                current = name
                stats[name] = {
                    "opbyte": int(m.group(2), 16),
                    "inputs": int(m.group(4)),
                    "total": int(m.group(3)),
                    "all_const": int(m.group(5)),
                    "const_output": int(m.group(7)) if m.group(7) else 0,
                    "per_input": [],
                }
                continue

            m = re.match(
                r"\s{4}\[(\d+)\]: const=(\d+)/(\d+) \(\d+%\), fits_usize=(\d+)/(\d+)",
                line,
            )
            if m and current:
                stats[current]["per_input"].append(
                    [int(m.group(3)), int(m.group(2)), int(m.group(4))]
                )
                continue

            if line.strip() and not line.startswith("  "):
                in_block = False
                current = None

        return stats

    @staticmethod
    def _merge(aggregate, stats):
        for name, s in stats.items():
            if name not in aggregate:
                aggregate[name] = {
                    "opbyte": s["opbyte"],
                    "inputs": s["inputs"],
                    "total": 0,
                    "all_const": 0,
                    "const_output": 0,
                    "per_input": [[0, 0, 0] for _ in range(s["inputs"])],
                }
            agg = aggregate[name]
            agg["total"] += s["total"]
            agg["all_const"] += s["all_const"]
            agg["const_output"] += s.get("const_output", 0)
            for i, [t, c, u] in enumerate(s["per_input"]):
                agg["per_input"][i][0] += t
                agg["per_input"][i][1] += c
                agg["per_input"][i][2] += u

    @classmethod
    def _print(cls, stats, title):
        print(f"\n=== {title} ===")
        for name in sorted(stats, key=lambda n: stats[n].get("opbyte", 0)):
            s = stats[name]
            t = s["total"]
            if t == 0:
                continue
            ac = s["all_const"]
            co = s.get("const_output", 0)
            line = (
                f"  {name:<16} {t} occ, {s['inputs']} inputs, "
                f"all_const={ac}/{t} ({cls._pct(ac, t)})"
            )
            if co:
                line += f", const_output={co}/{t} ({cls._pct(co, t)})"
            print(line)
            for i, [total, const, usize] in enumerate(s["per_input"]):
                if total > 0:
                    print(
                        f"    [{i}]: const={const}/{total} ({cls._pct(const, total)}), "
                        f"fits_usize={usize}/{total} ({cls._pct(usize, total)})"
                    )

    def report(self, benches, side):
        print("### Constant-input stats\n")
        aggregate = {}
        for bench in benches:
            stats = self._parse(side.outputs.get(bench, ""))
            if stats:
                self._merge(aggregate, stats)
                self._print(stats, bench_name(bench))
        self._print(aggregate, f"AGGREGATE ({len(benches)} benchmarks)")
        print()


# ---------------------------------------------------------------------------
# Collection
# ---------------------------------------------------------------------------


# Dev-profile CLI builds enable frame pointers through `debug_assertions`, and dumps force them
# on; release builds keep them off, so match those.
RUN_ARGS = ["--no-frame-pointers"]


def collect(side: Side, benches: list[str], need_codegen: bool, rust_log: str | None):
    """Run benchmarks once and store captured output per bench in ``side``.

    If need_codegen is set, runs full compilation with ``-o side.dump_dir``.
    Otherwise runs ``--parse-only``.
    When rust_log is set, stderr is captured too (trace output goes there).
    Failing benchmarks are recorded in ``side.failures`` instead of aborting.
    """
    env = cargo_env(rust_log) if rust_log else None
    label = "Compiling" if need_codegen else "Analyzing"
    for bench in tqdm(benches, desc=f"{label} {side.label}", unit="bench"):
        args = ["run", bench, *RUN_ARGS]
        if need_codegen:
            sub = os.path.join(side.dump_dir, dump_subdir(bench))
            if os.path.isdir(sub):
                shutil.rmtree(sub)
            args += ["-o", side.dump_dir]
        else:
            args.append("--parse-only")
        try:
            output = run_cli(side.binary, args, env, capture_stderr=rust_log is not None)
        except subprocess.CalledProcessError as e:
            side.failures[bench] = last_line(e.stderr)
            continue
        side.outputs[bench] = strip_ansi(output)


def measure(sides: list[Side], benches: list[str], runs: int, perf: bool):
    """Sample compile cost with ``run <bench> 0`` (compile, no dumps, no execution).

    Runs of the same bench on each side are adjacent, and the side order alternates
    every round, so load changes on the machine hit both sides alike.
    """
    if not perf:
        eprint("warning: `perf stat` is unavailable; recording wall time without instructions")
    with tqdm(total=runs * len(benches) * len(sides), desc="Timing", unit="run") as bar:
        for i in range(runs):
            order = sides if i % 2 == 0 else sides[::-1]
            for bench in benches:
                for side in order:
                    bar.update()
                    if bench in side.failures:
                        continue
                    try:
                        sample = time_cli(side.binary, ["run", bench, "0", *RUN_ARGS], perf)
                    except subprocess.CalledProcessError as e:
                        side.failures[bench] = last_line(e.stderr)
                        side.samples.pop(bench, None)
                        continue
                    side.samples.setdefault(bench, []).append(sample)


# Instructions executed per side and bench, split over `RUNTIME_SAMPLES` samples.
RUNTIME_TARGET = 20e9
RUNTIME_SAMPLES = 3
# Instructions a probe run must execute for its count to size the samples.
RUNTIME_PROBE = 1e8


def total_iters(n: int) -> int:
    """`evm2 run <bench> N` runs `max(N / 10, 10)` warmup iterations before the timed ones.

    Keep in sync with `bench` in `crates/cli/src/run.rs`.
    """
    return n + max(n // 10, 10)


def runtime_args(bench: str, n: int) -> list[str]:
    return ["run", bench, str(n), "--jit-only", *RUN_ARGS]


def sample_iters(side: Side, bench: str) -> int:
    """Return the `N` whose run executes about `RUNTIME_TARGET / RUNTIME_SAMPLES` instructions."""
    n = 10
    while (
        instructions := perf_counts(side.binary, runtime_args(bench, n))[0]
    ) < RUNTIME_PROBE and n < 10**7:
        n *= 10
    iters = int(RUNTIME_TARGET / RUNTIME_SAMPLES / (instructions / total_iters(n)))
    # Invert `total_iters`.
    return max(10, iters - 10 if iters < 110 else int(iters / 1.1))


def measure_runtime_bench(sides: list[Side], bench: str):
    """Measure execution instructions and cycles per iteration of one bench.

    Cycles move by several percent between runs of the same binary, so each side runs
    `RUNTIME_SAMPLES` shorter samples, interleaved across sides with the order alternating
    every round, and keeps the fewest cycles: noise only adds to them.
    """
    live = [side for side in sides if bench not in side.failures]
    iters = {}
    samples = {side.label: [] for side in live}
    for side in live[:]:
        try:
            iters[side.label] = sample_iters(side, bench)
        except subprocess.CalledProcessError as e:
            side.failures[bench] = last_line(e.stderr)
            live.remove(side)
    for i in range(RUNTIME_SAMPLES):
        for side in live[:] if i % 2 == 0 else live[::-1]:
            try:
                samples[side.label].append(
                    perf_counts(side.binary, runtime_args(bench, iters[side.label]))
                )
            except subprocess.CalledProcessError as e:
                side.failures[bench] = last_line(e.stderr)
                live.remove(side)
    for side in live:
        total = total_iters(iters[side.label])
        counts = samples[side.label]
        side.runtime[bench] = statistics.median(c[0] for c in counts) / total
        side.runtime_cycles[bench] = min(c[1] for c in counts) / total


def measure_runtime(sides: list[Side], benches: list[str]):
    """Measure execution instructions and cycles per iteration with `perf stat`."""
    if not perf_available():
        eprint("warning: `perf stat` is unavailable; skipping runtime")
        return
    for bench in tqdm(benches, desc="Runtime", unit="bench"):
        measure_runtime_bench(sides, bench)


def last_line(s: str | None) -> str:
    lines = [line.strip() for line in strip_ansi(s or "").splitlines() if line.strip()]
    return lines[-1] if lines else "unknown error"


def report_failures(sides: list[Side]):
    rows = [
        [side.label, bench_name(bench), err.replace("|", "\\|")]
        for side in sides
        for bench, err in side.failures.items()
    ]
    if rows:
        print("### Failures\n")
        print_table(["revision", "benchmark", "error"], rows, right_cols=0)


def git(root: str, *args: str) -> str:
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, check=True, cwd=root
    ).stdout.strip()


def ensure_private_dir(path: str):
    """Create ``path`` if needed, and refuse it unless only the current user can write to it.

    The script executes the cached base binary and builds the cached worktree, so another
    user must not be able to plant them, e.g. under a shared ``/tmp``.
    """
    os.makedirs(path, mode=0o700, exist_ok=True)
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o022:
        eprint(f"error: {path} must be a directory that only you can write to")
        raise SystemExit(1)


def base_binary(root: str, sha: str, base_dump: str) -> str:
    """Build the CLI at ``sha``, reusing the binary cached in ``base_dump`` when it matches.

    The base worktree lives in ``base_dump`` and must outlive the build: the CLI reads
    its fixtures from the source tree it was built from.
    """
    ensure_private_dir(base_dump)
    frozen = os.path.join(base_dump, ".evm2")
    stamp = os.path.join(base_dump, ".rev")
    worktree = os.path.join(base_dump, ".worktree")
    try:
        with open(stamp) as f:
            cached = f.read().strip()
    except FileNotFoundError:
        cached = None
    if cached == sha and os.path.isfile(frozen) and os.path.isdir(worktree):
        eprint(f"Reusing base binary for {sha[:12]}")
        return frozen

    if os.path.exists(stamp):
        os.remove(stamp)
    if os.path.isdir(worktree):
        git(worktree, "checkout", "--detach", "--force", "--quiet", sha)
    else:
        git(root, "worktree", "prune")
        git(root, "worktree", "add", "--detach", "--quiet", worktree, sha)
    # Cargo fingerprints workspace crates by paths relative to the workspace root, so a
    # target dir shared with the head would count unchanged-mtime head files as fresh.
    target = os.path.join(base_dump, "target")
    shutil.copy2(cargo_build(worktree, target_dir=target), frozen)
    with open(stamp, "w") as f:
        f.write(sha)
    return frozen


# ---------------------------------------------------------------------------
# Result export
# ---------------------------------------------------------------------------

COMPILER = "evm2"
# `results.json` keys of the codegen and compile-time table columns.
RESULT_KEYS = {
    "unopt.ll": "unopt_ll_lines",
    "opt.ll": "opt_ll_lines",
    "opt.s": "opt_s_lines",
    "jit size": "jit_size_bytes",
    "i256 loads": "i256_loads",
    "i256 stores": "i256_stores",
    "mresize": "mresize_calls",
    "spills": "spills",
    "reloads": "reloads",
    "instructions": "compile_instructions",
    "cycles": "compile_cycles",
    "time": "compile_time_seconds",
    **{p: f"{p}_seconds" for p in PHASES},
    RUNTIME_KEY: "runtime_instructions",
    RUNTIME_CYCLES_KEY: "runtime_cycles",
}
# Every metric in `results.json`, all lower-is-better; the perf site stores the declared ones.
METRICS = {
    "jit_size_bytes": {"title": "JIT size", "unit": "bytes"},
    "opt_s_lines": {"title": "Assembly lines", "unit": "lines"},
    "opt_ll_lines": {"title": "Optimized IR lines", "unit": "lines"},
    "unopt_ll_lines": {"title": "Unoptimized IR lines", "unit": "lines"},
    "spills": {"title": "Spills", "unit": "spills"},
    "reloads": {"title": "Reloads", "unit": "reloads"},
    "i256_loads": {"title": "i256 loads", "unit": "loads"},
    "i256_stores": {"title": "i256 stores", "unit": "stores"},
    "mresize_calls": {"title": "mresize calls", "unit": "calls"},
    "compile_instructions": {"title": "Compile instructions", "unit": "instructions"},
    "compile_cycles": {"title": "Compile cycles", "unit": "cycles"},
    "compile_time_seconds": {"title": "Compile time", "unit": "seconds"},
    **{f"{p}_seconds": {"title": f"{p.capitalize()} time", "unit": "seconds"} for p in PHASES},
    "runtime_instructions": {"title": "Runtime instructions", "unit": "instructions"},
    "runtime_cycles": {"title": "Runtime cycles", "unit": "cycles"},
}
ARTIFACT_FILES = ["bytecode.txt", "bytecode.dot", "opt.ll", "opt.s"]
TARGET_CPU_RE = re.compile(r'"target-cpu"="([^"]+)"')
BENCH_FIXTURE_RE = re.compile(r'Bench::\w+\(\s*"([^"]+)",[^"]*"([^"]+)"')


def source_links(root: str, rev: str) -> dict[str, list[dict[str, str]]]:
    """Link each built-in benchmark to its fixture in the benchmark catalog."""
    try:
        with open(os.path.join(root, "crates/cli/src/evm_bench.rs")) as f:
            catalog = f.read()
    except FileNotFoundError:
        return {}
    repo = os.environ.get("GITHUB_REPOSITORY", "alloy-rs/evm2")
    return {
        name: [{"label": path, "url": f"https://github.com/{repo}/blob/{rev}/{path}"}]
        for name, path in BENCH_FIXTURE_RE.findall(catalog)
    }


def target_cpu(side: Side, benches: list[str]) -> str | None:
    for bench in benches:
        try:
            with open(os.path.join(side.dump_dir, dump_subdir(bench), "opt.ll")) as f:
                if m := TARGET_CPU_RE.search(f.read()):
                    return m.group(1)
        except FileNotFoundError:
            continue
    return None


def write_results(out_dir: str, benches: list[str], side: Side, perf: bool):
    """Write ``results.json`` and normalized artifacts for the perf site and ``compare.py``."""
    codegen = CodegenLines()._collect(benches, side.dump_dir)
    times = CompileTime()._collect(benches, side)
    links = source_links(side.root, side.rev)
    artifacts = os.path.join(out_dir, "artifacts")
    if os.path.isdir(artifacts):
        shutil.rmtree(artifacts)
    results = []
    for bench in benches:
        test_id = dump_subdir(bench)
        name = bench_name(bench)
        result = {"test_id": test_id, "suite": "jit"}
        if test_id in links:
            result["source_links"] = links[test_id]
        if bench in side.failures or name not in codegen:
            error = side.failures.get(bench, "no codegen output")
            result["compilers"] = {COMPILER: {"status": "failed", "error": error}}
            results.append(result)
            continue
        metrics = {**codegen[name], **times.get(name, {})}
        counts = ("instructions", "cycles")
        values = {RESULT_KEYS[k]: v for k, v in metrics.items() if v or k not in counts}
        if bench in side.runtime:
            values["runtime_instructions"] = round(side.runtime[bench])
            values["runtime_cycles"] = round(side.runtime_cycles[bench])
        result["compilers"] = {COMPILER: {"status": "ok", **values}}
        results.append(result)

        src = os.path.join(side.dump_dir, test_id)
        dst = os.path.join(artifacts, test_id, COMPILER)
        os.makedirs(dst)
        for file in ARTIFACT_FILES:
            try:
                with open(os.path.join(src, file)) as f:
                    text = f.read()
            except FileNotFoundError:
                continue
            with open(os.path.join(dst, file), "w") as f:
                f.write(normalize_dump(text, side.dump_dir))
        with open(os.path.join(src, "bytecode.bin"), "rb") as f:
            code = f.read()
        with open(os.path.join(dst, "bytecode.hex"), "w") as f:
            f.write(code.hex() + "\n")

    document = {
        "schema_version": 1,
        "commit": side.rev,
        "environment": {"target_cpu": target_cpu(side, benches), "perf_counters": perf},
        "metrics": METRICS,
        "results": results,
    }
    with open(os.path.join(out_dir, "results.json"), "w") as f:
        json.dump(document, f, indent=2)
        f.write("\n")
    eprint(f"Wrote {os.path.join(out_dir, 'results.json')}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(
        description="Unified benchmark tool for evm2-jit.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("dump_dir", help="Directory to dump generated files into")
    parser.add_argument("benches", nargs="*", help="Specific benchmarks to run (default: all)")
    parser.add_argument(
        "--diff",
        metavar="REV",
        dest="base_rev",
        help="Git revision to compare against (e.g. main)",
    )
    parser.add_argument(
        "--extra-dir",
        action="append",
        default=None,
        help="Extra directory with .bin files to benchmark (can be repeated). "
        "Defaults to ['tmp/mainnet'] when running all benchmarks; "
        "pass any --extra-dir to override or to include extras with explicit "
        "benchmarks.",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=3,
        help="Compile-cost samples per benchmark and revision (default: 3)",
    )
    parser.add_argument(
        "--output",
        metavar="DIR",
        help="Write head results.json and artifacts for compare.py and the perf site",
    )
    parser.add_argument(
        "--base-output",
        metavar="DIR",
        help="Like --output, for the --diff base",
    )

    # Analysis selectors. Default: --codegen-lines --compile-times.
    parser.add_argument(
        "--codegen-lines",
        action="store_true",
        help="Assembly line count tables (default)",
    )
    parser.add_argument(
        "--compile-times",
        action="store_true",
        help="Compile time tables (default)",
    )
    parser.add_argument(
        "--jump-resolution",
        action="store_true",
        help="Report dynamic jump resolution stats",
    )
    parser.add_argument(
        "--input-stats",
        action="store_true",
        help="Report per-opcode constant-input statistics",
    )
    parser.add_argument(
        "--runtime",
        action="store_true",
        help="Report execution instructions per iteration (slow; needs perf)",
    )
    parser.add_argument(
        "--block-stats",
        action="store_true",
        help="Report IR stats (inst counts, block size distribution, message opcodes)",
    )
    args = parser.parse_args()
    if args.base_output and not args.base_rev:
        parser.error("--base-output requires --diff")
    # Absolute, so the dump path embedded in debug info is known.
    args.dump_dir = os.path.abspath(args.dump_dir)
    ensure_private_dir(args.dump_dir)

    # Default to codegen-lines + compile-times if no analysis flags given; --output needs both.
    if args.output or args.base_output:
        args.codegen_lines = True
        args.compile_times = True
    if not (
        args.codegen_lines
        or args.compile_times
        or args.jump_resolution
        or args.input_stats
        or args.block_stats
        or args.runtime
    ):
        args.codegen_lines = True
        args.compile_times = True

    # Build the list of active analyses.
    analyses: list[Analysis] = []
    if args.codegen_lines:
        analyses.append(CodegenLines())
    if args.compile_times:
        analyses.append(CompileTime())
    if args.jump_resolution:
        analyses.append(JumpResolution())
    if args.input_stats:
        analyses.append(InputStats())
    if args.block_stats:
        analyses.append(BlockStats())
    if args.runtime:
        analyses.append(Runtime())

    if not analyses:
        eprint("No analyses selected.")
        return

    # Determine collection requirements from the union of all analyses.
    need_codegen = any(a.needs_codegen() for a in analyses)
    need_timing = any(a.needs_timing() for a in analyses)
    need_runtime = any(a.needs_runtime() for a in analyses)
    rust_log_parts = {r for a in analyses if (r := a.rust_log())}
    rust_log = ",".join(sorted(rust_log_parts)) if rust_log_parts else None

    root = repo_root()
    head_sha = git(root, "rev-parse", "HEAD")
    is_dirty = git(root, "status", "--porcelain") != ""

    if args.base_rev:
        # Abort if base_rev resolves to HEAD and there are no working-tree changes to compare.
        base_sha = git(root, "rev-parse", "--verify", f"{args.base_rev}^{{commit}}")
        if head_sha == base_sha and not is_dirty:
            eprint(
                f"error: --diff {args.base_rev!r} resolves to HEAD ({head_sha[:12]}); "
                f"diffing a branch against itself is pointless"
            )
            raise SystemExit(1)

    base = None
    if args.base_rev:
        base_dump = args.dump_dir + ".base"
        base_root = os.path.join(base_dump, ".worktree")
        base = Side("base", base_sha, base_binary(root, base_sha, base_dump), base_dump, base_root)
    head = Side("head", head_sha, cargo_build(root), args.dump_dir, root)
    builtin = args.benches or get_benches(head.binary)
    extra_dirs = (
        args.extra_dir if args.extra_dir is not None else ([] if args.benches else ["tmp/mainnet"])
    )
    extra = find_extra_benches(extra_dirs, root)
    benches = builtin + extra

    if not benches:
        eprint("No benchmarks found.")
        return

    eprint(f"Benchmarks: {len(builtin)} built-in + {len(extra)} extra")

    sides = [head, base] if args.base_rev else [head]
    for side in sides:
        collect(side, benches, need_codegen, rust_log)
    perf = need_timing and perf_available()
    if need_timing:
        measure(sides, benches, args.runs, perf)
    if need_runtime:
        measure_runtime(sides, benches)

    results_path = os.path.join(args.dump_dir, "results.md")
    with open(results_path, "w") as md:
        old_stdout = sys.stdout
        sys.stdout = _Tee(old_stdout, md)
        try:
            head_desc = f"`{head.rev[:10]}`" + (" with uncommitted changes" if is_dirty else "")
            if args.base_rev:
                print(f"Head {head_desc} vs base `{args.base_rev}` (`{base.rev[:10]}`).\n")
            else:
                print(f"Head {head_desc}.\n")
            report_failures(sides)
            for a in analyses:
                if args.base_rev:
                    a.report_diff(benches, head, base)
                else:
                    a.report(benches, head)
        finally:
            sys.stdout = old_stdout
    eprint(f"Wrote {results_path}")

    for side, out_dir in [(head, args.output), (base, args.base_output)]:
        if out_dir:
            os.makedirs(out_dir, exist_ok=True)
            write_results(out_dir, benches, side, perf)


if __name__ == "__main__":
    main()
