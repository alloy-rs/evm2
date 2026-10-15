#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.13"
# dependencies = ["tabulate>=0.9", "tqdm>=4.67.3"]
# ///
"""Compare `bench.py --output` runs and write the CI report and PR comment.

Inputs are `results.json` paths or the directories holding them; artifacts are read from
`artifacts/` beside each. Numeric regressions do not fail; missing or invalid inputs do.

Examples:
    ./scripts/jit/compare.py /tmp/base-out /tmp/head-out
    ./scripts/jit/compare.py --results target/jit-bench --pr-comment-output comment.md
"""

import argparse
import contextlib
import hashlib
import io
import json
import os
import statistics
import subprocess
import sys
import uuid
from pathlib import Path
from urllib.parse import urlencode

from bench import (
    CODEGEN_COLUMNS,
    COMPILER,
    METRICS,
    NOISE_CODEGEN,
    RESULT_KEYS,
    SUMMARY_TIME_KEYS,
    TIME_KEYS,
    fmt_codegen,
    fmt_time_key,
    is_noise,
    print_diff,
    print_solo,
    print_table,
    time_changed,
)

PERF_SITE_URL = "https://www.getfoundry.sh/perf/evm2/"
# Metrics whose changes are deterministic and decide whether a benchmark changed.
CODEGEN_METRICS = ["jit_size_bytes", "opt_s_lines", "opt_ll_lines", "unopt_ll_lines"]


def load(path: Path) -> dict:
    if path.is_dir():
        path = path / "results.json"
    document = json.loads(path.read_text())
    if not isinstance(document, dict) or not isinstance(document.get("results"), list):
        raise ValueError(f"{path}: expected a bench.py results document")
    document["path"] = path
    return document


def compiler(result: dict) -> dict:
    return result.get("compilers", {}).get(COMPILER, {})


def ok_results(document: dict | None) -> dict[str, dict]:
    if document is None:
        return {}
    return {
        r["test_id"]: compiler(r) for r in document["results"] if compiler(r).get("status") == "ok"
    }


def failures(document: dict) -> dict[str, str]:
    return {
        r["test_id"]: compiler(r).get("error", "unknown error")
        for r in document["results"]
        if compiler(r).get("status") != "ok"
    }


def artifact_hashes(document: dict, test_id: str) -> dict[str, str]:
    root = document["path"].parent / "artifacts" / test_id / COMPILER
    if not root.is_dir():
        return {}
    return {
        str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(root.rglob("*"))
        if p.is_file()
    }


def pct(before, after) -> float | None:
    if before is None or after is None or before == 0:
        return None
    return (after - before) / before * 100


def geomean_pct(pairs: list[tuple]) -> float | None:
    ratios = [a / b for b, a in pairs if b and a and b > 0 and a > 0]
    if not ratios:
        return None
    return (statistics.geometric_mean(ratios) - 1) * 100


def fmt_change(value: float | None) -> str:
    if value is None:
        return "n/a"
    rounded = round(value, 2)
    if rounded == 0:
        return "~0%"
    return f"{'❌' if rounded > 0 else '✅'} {rounded:+.2f}%"


def compile_metric(head: dict, base: dict | None) -> str:
    """Retired instructions when both runs counted them, else (noisy) wall time."""
    docs = [head] if base is None else [head, base]
    if all(d.get("environment", {}).get("perf_counters") for d in docs):
        return "compile_instructions"
    return "compile_time_seconds"


def compare(head: dict, base: dict | None) -> dict:
    head_ok = ok_results(head)
    base_ok = ok_results(base)
    metrics = [*CODEGEN_METRICS, compile_metric(head, base)]
    rows = []
    for test_id, after in head_ok.items():
        before = base_ok.get(test_id)
        row = {"test_id": test_id, "metrics": {}, "artifacts": []}
        for name in metrics:
            b = before.get(name) if before else None
            a = after.get(name)
            row["metrics"][name] = {"before": b, "after": a, "percent": pct(b, a)}
        if before is not None:
            old = artifact_hashes(base, test_id)
            new = artifact_hashes(head, test_id)
            if old and new:
                for path in sorted(old.keys() | new.keys()):
                    if old.get(path) != new.get(path):
                        status = (
                            "added"
                            if path not in old
                            else "removed"
                            if path not in new
                            else "changed"
                        )
                        row["artifacts"].append({"path": path, "status": status})
        row["changed"] = before is not None and (
            bool(row["artifacts"])
            or any(
                row["metrics"][m]["before"] != row["metrics"][m]["after"] for m in CODEGEN_METRICS
            )
        )
        rows.append(row)
    summary = {
        name: geomean_pct(
            [
                (r["metrics"][name]["before"], r["metrics"][name]["after"])
                for r in rows
                if r["test_id"] in base_ok
            ]
        )
        for name in metrics
    }
    return {
        "metrics": metrics,
        "rows": rows,
        "summary": summary,
        "failures": failures(head),
        "missing": sorted(base_ok.keys() - {r["test_id"] for r in head["results"]}),
        "asm_changed": sum(
            any(a["path"] == "opt.s" for a in r["artifacts"])
            for r in rows
            if r["test_id"] in base_ok
        ),
        "compared": sum(r["test_id"] in base_ok for r in rows),
    }


def asm_summary(comparison: dict) -> str:
    return f"{comparison['asm_changed']}/{comparison['compared']}"


def table_rows(document: dict | None, columns: list[str]) -> dict[str, dict]:
    return {
        test_id: {c: values.get(RESULT_KEYS[c]) or 0 for c in columns}
        for test_id, values in ok_results(document).items()
    }


def detailed_report(head: dict, base: dict | None, comparison: dict, base_ref: str) -> str:
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        head_sha = head.get("commit", "")[:10]
        if base is None:
            print(f"Head `{head_sha}`.\n")
        else:
            print(f"Head `{head_sha}` vs base `{base_ref}` (`{base.get('commit', '')[:10]}`).\n")
        env = head.get("environment", {})
        print(f"Target CPU: `{env.get('target_cpu')}`.\n")
        if comparison["failures"]:
            print("### Failures\n")
            print_table(
                ["benchmark", "error"],
                [[t, e.replace("|", "\\|")] for t, e in comparison["failures"].items()],
                right_cols=0,
            )
        if base is None:
            print("### Codegen statistics\n")
            print_solo(table_rows(head, CODEGEN_COLUMNS), CODEGEN_COLUMNS, fmt_codegen)
            print("### Compile times\n")
            print_solo(table_rows(head, TIME_KEYS), TIME_KEYS, fmt_time_key)
        else:
            print("### Codegen statistics\n")
            print(f"`opt.s` changed in {asm_summary(comparison)} benchmarks.\n")
            print_diff(
                table_rows(head, CODEGEN_COLUMNS),
                table_rows(base, CODEGEN_COLUMNS),
                CODEGEN_COLUMNS,
                fmt_codegen,
                lambda ps: not all(is_noise(b, c, NOISE_CODEGEN) for b, c in ps.values()),
                "Full details",
            )
            print("### Compile times\n")
            if compile_metric(head, base) == "compile_time_seconds":
                print("Wall times from separate runs; expect noise.\n")
            print_diff(
                table_rows(head, TIME_KEYS),
                table_rows(base, TIME_KEYS),
                TIME_KEYS,
                fmt_time_key,
                time_changed,
                "Full compile times",
                SUMMARY_TIME_KEYS,
            )
            artifacts = [
                [r["test_id"], ", ".join(f"`{a['path']}` {a['status']}" for a in r["artifacts"])]
                for r in comparison["rows"]
                if r["artifacts"]
            ]
            if artifacts:
                print("### Artifact changes\n")
                print_table(["benchmark", "files"], artifacts, right_cols=0)
    return out.getvalue()


def perf_link(label: str, benchmark: str | None = None, section: str = "benchmarks") -> str:
    base = os.environ.get("BENCHMARK_BASE_SHA")
    head = os.environ.get("BENCHMARK_PR_HEAD_SHA")
    if not base or not head:
        return label
    query = {"base": base, "head": head}
    if benchmark is not None:
        query["benchmark"] = benchmark
        section = "artifacts"
    site = os.environ.get("BENCHMARK_SITE_URL") or PERF_SITE_URL
    return f"[{label}]({site}?{urlencode(query)}#{section})"


def pr_comment(comparison: dict, has_baseline: bool, behind_base: bool, base_ref: str) -> str:
    metrics = comparison["metrics"]
    titles = {name: METRICS[name]["title"] for name in metrics}
    lines = ["## JIT benchmarks", ""]
    changed = [r for r in comparison["rows"] if r["changed"]]
    if not comparison["rows"]:
        lines.append("No benchmark results were produced.")
    elif not has_baseline:
        lines.append("No baseline was available for comparison.")
    else:
        lines.append(
            f"Benchmark changes detected against `{base_ref}`."
            if changed
            else f"No codegen changes against `{base_ref}`."
        )
        lines += ["", "### Overview", "", "| Metric | Change |", "| --- | ---: |"]
        lines += [f"| {titles[m]} | {fmt_change(comparison['summary'][m])} |" for m in metrics]
        lines += [
            "",
            "Equal-weight geometric means; lower is better. "
            f"`opt.s` changed in {asm_summary(comparison)} benchmarks.",
        ]
        if changed:
            lines += [
                "",
                f"### Changed benchmarks vs `{base_ref}`",
                "",
                "| Benchmark | " + " | ".join(titles[m] for m in metrics) + " |",
                "| --- |" + " ---: |" * len(metrics),
            ]
            for row in changed:
                cells = [perf_link(row["test_id"], row["test_id"])]
                cells += [fmt_change(row["metrics"][m]["percent"]) for m in metrics]
                lines.append("| " + " | ".join(cells) + " |")
    if comparison["failures"]:
        names = ", ".join(f"`{t}`" for t in comparison["failures"])
        lines += ["", f"> ⚠️ Failed benchmarks: {names}."]
    if comparison["missing"]:
        names = ", ".join(f"`{t}`" for t in comparison["missing"])
        lines += ["", f"> ⚠️ Missing benchmarks: {names}."]
    if behind_base:
        lines += ["", f"> ⚠️ This branch is behind `{base_ref}`; results may be stale."]
    button = "![View benchmark overview](https://img.shields.io/badge/View_benchmark_overview-2563eb?style=for-the-badge)"
    link = perf_link(button)
    if link == button:
        link = f"[{button}]({os.environ.get('BENCHMARK_SITE_URL') or PERF_SITE_URL})"
    lines += ["", link, ""]
    return "\n".join(lines)


def branch_is_behind(base_ref: str) -> bool:
    head = os.environ.get("BENCHMARK_PR_HEAD_SHA")
    if not head:
        return False
    try:
        count = subprocess.check_output(
            ["git", "rev-list", "--count", f"{head}..origin/{base_ref}"],
            text=True,
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.CalledProcessError):
        return False
    return int(count) > 0


def append_github_output(name: str, value: str):
    if path := os.environ.get("GITHUB_OUTPUT"):
        delimiter = f"benchmark_{uuid.uuid4().hex}"
        with open(path, "a") as f:
            f.write(f"{name}<<{delimiter}\n{value}\n{delimiter}\n")


def write(path: Path | None, text: str):
    if path is not None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("runs", nargs="*", type=Path, metavar="RUN", help="Baseline and head runs")
    parser.add_argument("--results", type=Path, help="Head run")
    parser.add_argument("--baseline", type=Path, help="Baseline run")
    parser.add_argument("--report-output", type=Path, help="Write the detailed Markdown report")
    parser.add_argument("--pr-comment-output", type=Path, help="Write the compact PR comment")
    parser.add_argument("--json-output", type=Path, help="Write per-benchmark deltas")
    parser.add_argument("--comment-output", type=Path, help="Write the should-comment flag")
    args = parser.parse_args(argv)
    if args.runs:
        if len(args.runs) != 2 or args.results or args.baseline:
            parser.error("provide two runs, or use --baseline and --results")
        args.baseline, args.results = args.runs
    if args.results is None:
        parser.error("provide baseline and head runs, or --results")
    try:
        head = load(args.results)
        base = load(args.baseline) if args.baseline else None
    except (OSError, ValueError) as e:
        parser.error(str(e))

    base_ref = os.environ.get("BENCHMARK_BASE_REF") or "main"
    comparison = compare(head, base)
    should_comment = (
        base is None
        or bool(comparison["failures"] or comparison["missing"])
        or any(r["changed"] for r in comparison["rows"])
    )
    behind = branch_is_behind(base_ref)
    report = detailed_report(head, base, comparison, base_ref)
    print(report)
    write(args.report_output, report)
    write(args.pr_comment_output, pr_comment(comparison, base is not None, behind, base_ref))
    write(args.json_output, json.dumps(comparison, indent=2) + "\n")
    write(args.comment_output, f"{str(should_comment).lower()}\n")
    append_github_output("should_comment", str(should_comment).lower())
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as f:
            f.write(report + "\n")
    for test_id, error in comparison["failures"].items():
        print(f"::warning::{test_id} failed: {error}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
