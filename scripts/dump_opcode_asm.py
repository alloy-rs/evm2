#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.13"
# dependencies = ["tqdm>=4.67.3"]
# ///
"""Dump cargo-asm output for EVM opcode dispatch functions.

Examples:
    ./scripts/dump_opcode_asm.py
    ./scripts/dump_opcode_asm.py ADD PUSH1 SSTORE -o tmp/mydump
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

from tqdm import tqdm
from utils import cargo_env, repo_root


ROOT = Path(repo_root())
OPCODE_RS = ROOT / "crates" / "evm2" / "src" / "interpreter" / "opcode.rs"
DEFAULT_OUT = ROOT / "tmp" / "dump"
DISPATCH = "evm2::interpreter::instructions::table::dispatch"
CONFIG = "evm2::config::EvmVersion<(), {spec}>"


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
        "--spec",
        type=int,
        default=19,
        help="EvmVersion const SPEC used for monomorphized dispatch. Defaults to 19.",
    )
    parser.add_argument(
        "--package",
        default="evm2",
        help="Cargo package passed to cargo asm. Defaults to evm2.",
    )
    return parser.parse_args()


def select_opcodes(opcodes: dict[str, int], mnemonics: list[str]) -> list[tuple[str, int]]:
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
        raise SystemExit(f"unknown opcode mnemonic(s): {' '.join(missing)}\nknown: {known}")
    return selected


def cargo_asm(package: str, spec: int, opcode: int) -> str:
    symbol = f"{DISPATCH}::<{CONFIG.format(spec=spec)}, {opcode}>"
    cmd = ["cargo", "asm", "-q", "-s", "-p", package, "--lib", symbol]
    proc = subprocess.run(
        cmd,
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=cargo_env(),
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"cargo asm failed for opcode {opcode:#04x}\n"
            f"command: {' '.join(cmd)}\n"
            f"stdout:\n{proc.stdout}\n"
            f"stderr:\n{proc.stderr}"
        )
    return proc.stdout


def main() -> int:
    args = parse_args()
    opcodes = parse_opcodes()
    selected = select_opcodes(opcodes, args.mnemonics)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)

    total = len(selected)
    progress = tqdm(selected, total=total, unit="opcode", dynamic_ncols=True)
    for mnemonic, opcode in progress:
        progress.set_description(mnemonic)
        asm = cargo_asm(args.package, args.spec, opcode)
        path = out / f"{opcode:02x}_{mnemonic}.s"
        path.write_text(asm)

    print(f"wrote {total} asm file(s) to {out.relative_to(ROOT)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
