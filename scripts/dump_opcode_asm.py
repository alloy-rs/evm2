#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.13"
# dependencies = []
# ///
"""Dump cargo-asm output for EVM opcode dispatch functions.

Examples:
    ./scripts/dump_opcode_asm.py
    ./scripts/dump_opcode_asm.py ADD PUSH1 SSTORE -o tmp/mydump
    ./scripts/dump_opcode_asm.py --features evm2/nightly ADD
"""

import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

from utils import cargo_env, repo_root

ROOT = Path(repo_root())
OPCODE_RS = ROOT / "crates" / "evm2" / "src" / "interpreter" / "opcode.rs"
DEFAULT_OUT = ROOT / "tmp" / "dump"
DISPATCH_SYMBOLS = (
    "evm2::interpreter::instructions::table::dispatch::<",
    "evm2::interpreter::instructions::table::tail_dispatch::<",
)
DISPATCH_OPCODE = re.compile(r",\s*(\d+)(?:,\s*(?:true|false))?>")


def log(message: str) -> None:
    print(f"[dump_opcode_asm] {message}", file=sys.stderr, flush=True)


def parse_opcodes() -> dict[str, int]:
    opcodes = {}
    pattern = re.compile(r"^\s*(0x[0-9A-Fa-f]{2})\s*=>\s*([A-Z0-9_]+)\s*=>")
    for line in OPCODE_RS.read_text().splitlines():
        match = pattern.match(line)
        if match:
            value, mnemonic = match.groups()
            opcodes[mnemonic] = int(value, 16)
    return opcodes


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Dump cargo-asm output for EVM opcode dispatch functions."
    )
    parser.add_argument(
        "mnemonics",
        nargs="*",
        help="Opcode mnemonics to dump, e.g. ADD PUSH1 SSTORE. Defaults to all known opcodes.",
    )
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        default=DEFAULT_OUT,
        help="Output directory. Defaults to ./tmp/dump.",
    )
    parser.add_argument(
        "--package",
        default="evm2-statetest",
        help="Cargo package passed to cargo asm. Defaults to evm2-statetest.",
    )
    parser.add_argument(
        "-F",
        "--features",
        action="append",
        default=[],
        help="Cargo feature(s) passed through to cargo asm. Can be repeated.",
    )
    parser.add_argument(
        "--keep-everything",
        action="store_true",
        help="Keep the full cargo asm --everything dumps in the output directory.",
    )
    parser.add_argument(
        "--all-monomorphizations",
        action="store_true",
        help="Dump every matching dispatch monomorphization instead of only the first.",
    )
    return parser.parse_args()


def select_opcodes(
    opcodes: dict[str, int], mnemonics: list[str]
) -> list[tuple[str, int]]:
    if not mnemonics:
        return sorted(opcodes.items(), key=lambda item: item[1])

    selected = []
    missing = []
    for mnemonic in mnemonics:
        key = mnemonic.upper()
        if key in opcodes:
            selected.append((key, opcodes[key]))
        else:
            missing.append(mnemonic)

    if missing:
        known = " ".join(sorted(opcodes))
        raise SystemExit(
            f"unknown opcode mnemonic(s): {' '.join(missing)}\nknown: {known}"
        )
    return selected


def cargo_asm_everything(package: str, features: list[str], output: str) -> str:
    cmd = ["cargo", "asm", "-q", "-s", "--simplify", "-p", package]
    for feature in features:
        cmd.extend(("-F", feature))
    cmd.extend(("--lib", f"--{output}", "--everything"))
    log(f"running {' '.join(cmd)}")
    started = time.perf_counter()
    proc = subprocess.run(
        cmd,
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=cargo_env(),
        check=False,
    )
    elapsed = time.perf_counter() - started
    if proc.returncode != 0:
        log(f"failed after {elapsed:.1f}s: cargo asm --{output} --everything")
        raise RuntimeError(
            f"cargo asm --{output} --everything failed\n"
            f"command: {' '.join(cmd)}\n"
            f"stdout:\n{proc.stdout}\n"
            f"stderr:\n{proc.stderr}"
        )
    log(f"finished cargo asm --{output} --everything in {elapsed:.1f}s")
    return proc.stdout


def dispatch_opcode(text: str) -> int | None:
    if not any(symbol in text for symbol in DISPATCH_SYMBOLS):
        return None
    match = DISPATCH_OPCODE.search(text)
    if match is None:
        return None
    return int(match.group(1))


def is_asm_symbol_label(line: str) -> bool:
    return (
        line.endswith(":\n")
        and not line.startswith(("\t", " ", ".", "#"))
        and len(line) > 2
    )


def extract_asm_functions(text: str) -> dict[int, list[str]]:
    lines = text.splitlines(keepends=True)
    blocks: dict[int, list[str]] = {}
    i = 0
    while i < len(lines):
        line = lines[i]
        opcode = dispatch_opcode(line) if line.startswith(".section ") else None
        if opcode is not None:
            start = i
            i += 1
            while i < len(lines) and not lines[i].startswith(".section "):
                i += 1
            block = lines[start:i]
            if any(line.startswith(".type\t") for line in block):
                blocks.setdefault(opcode, []).append(clean_asm_block(block).rstrip() + "\n")
            continue

        opcode = dispatch_opcode(line) if is_asm_symbol_label(line) else None
        if opcode is not None:
            start = i
            i += 1
            while i < len(lines) and not is_asm_symbol_label(lines[i]):
                i += 1
            block = lines[start:i]
            blocks.setdefault(opcode, []).append(clean_asm_block(block).rstrip() + "\n")
            continue
        i += 1
    return blocks


def clean_asm_block(lines: list[str]) -> str:
    return "".join(line for line in lines if not re.match(r"\.Ltmp\d+:\n?$", line))


def extract_llvm_functions(text: str) -> dict[int, list[str]]:
    lines = text.splitlines(keepends=True)
    blocks: dict[int, list[str]] = {}
    i = 0
    while i < len(lines):
        line = lines[i]
        opcode = dispatch_opcode(line) if line.startswith("; ") else None
        if opcode is not None:
            start = i
            i += 1
            while i < len(lines) and not lines[i].startswith("define "):
                i += 1
            if i == len(lines):
                break
            i += 1
            while i < len(lines):
                if lines[i].startswith("}"):
                    i += 1
                    break
                i += 1
            blocks.setdefault(opcode, []).append("".join(lines[start:i]).rstrip() + "\n")
            continue
        i += 1
    return blocks


def extract_functions(text: str, output: str) -> dict[int, list[str]]:
    if output == "asm":
        return extract_asm_functions(text)
    if output == "llvm":
        return extract_llvm_functions(text)
    raise ValueError(f"unsupported cargo asm output: {output}")


def dump_output(
    out: Path,
    blocks_by_opcode: dict[int, list[str]],
    mnemonic: str,
    opcode: int,
    output: str,
    all_monomorphizations: bool,
) -> Path:
    blocks = blocks_by_opcode.get(opcode, [])
    if not blocks:
        raise RuntimeError(
            f"could not find cargo asm --{output} output for {mnemonic} ({opcode:#04x})"
        )
    if not all_monomorphizations and len(blocks) > 1:
        log(f"{mnemonic} has {len(blocks)} {output} monomorphization(s); writing the first")
        blocks = blocks[:1]

    suffix = "ll" if output == "llvm" else "s"
    path = out / f"{mnemonic}.{suffix}"
    path.write_text("\n\n".join(blocks))
    return path


def main() -> int:
    args = parse_args()
    opcodes = parse_opcodes()
    selected = select_opcodes(opcodes, args.mnemonics)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    feature_msg = f" with features {', '.join(args.features)}" if args.features else ""
    log(
        f"dumping {len(selected)} opcode(s) from package {args.package}{feature_msg} "
        f"to {out.relative_to(ROOT)}"
    )

    dumps: dict[str, dict[int, list[str]]] = {}
    for output in ("asm", "llvm"):
        text = cargo_asm_everything(args.package, args.features, output)
        if args.keep_everything:
            suffix = "ll" if output == "llvm" else "s"
            log(f"writing full cargo asm --{output} dump")
            (out / f"everything.{suffix}").write_text(text)
        dumps[output] = extract_functions(text, output)
        block_count = sum(len(blocks) for blocks in dumps[output].values())
        log(f"extracted {block_count} dispatch block(s) from --{output} output")

    tasks = [
        (mnemonic, opcode, output)
        for mnemonic, opcode in selected
        for output in ("asm", "llvm")
    ]
    for mnemonic, opcode, output in tasks:
        path = dump_output(
            out,
            dumps[output],
            mnemonic,
            opcode,
            output,
            args.all_monomorphizations,
        )
        log(f"wrote {path.relative_to(ROOT)}")
    print(f"wrote {len(tasks)} file(s) to {out.relative_to(ROOT)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
