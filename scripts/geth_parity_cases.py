#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["eth-keys>=0.5", "eth-utils>=5", "pycryptodome>=3.20"]
# ///
"""Generates the seed cases of the geth parity harness.

Every case is a directory with the inputs of `evm t8n` (`alloc.json`, `env.json`, `txs.json`) and a
`case.json` that names the forks and, optionally, the tracers and whether the node runs it. The
generated files are committed; run this script to change or add a case, then `--bless` the case
with `scripts/geth_parity.py`.

    uv run scripts/geth_parity_cases.py            # regenerate every case
    uv run scripts/geth_parity_cases.py call_      # only cases whose name contains `call_`
"""

import argparse
import json
import re
import shutil
import sys
from pathlib import Path

from eth_keys import keys
from eth_utils import keccak

ROOT = Path(__file__).resolve().parent.parent
CASES = ROOT / "crates/cli/testdata/geth-parity/cases"

FORKS = ["Cancun", "Prague", "Osaka", "Amsterdam"]
PRAGUE = ["Prague", "Osaka", "Amsterdam"]

SENDER_KEY = "0x45a915e4d060149eb4365960e6a7a45f334393093061116b197e3240065ff2d8"
SENDER = "0xa94f5374fce5edbc8e2a8697c15331677e6ebf0b"
COINBASE = "0x2adc25665018aa1fe0e6bc666dac8fc2697ff9ba"
ETHER = 10**18
ZERO_ADDR = "0x" + "00" * 20

A1, A2, A3, A4, A5 = (f"0x{n:040x}" for n in (0x1000001, 0x1000002, 0x1000003, 0x1000004, 0x1000005))
EMPTY = "0x" + "00" * 19 + "99"

OPS = {
    "STOP": 0x00, "ADD": 0x01, "MUL": 0x02, "SUB": 0x03, "DIV": 0x04, "SDIV": 0x05, "MOD": 0x06,
    "SMOD": 0x07, "ADDMOD": 0x08, "MULMOD": 0x09, "EXP": 0x0A, "SIGNEXTEND": 0x0B, "LT": 0x10,
    "GT": 0x11, "SLT": 0x12, "SGT": 0x13, "EQ": 0x14, "ISZERO": 0x15, "AND": 0x16, "OR": 0x17,
    "XOR": 0x18, "NOT": 0x19, "BYTE": 0x1A, "SHL": 0x1B, "SHR": 0x1C, "SAR": 0x1D, "CLZ": 0x1E,
    "KECCAK256": 0x20, "ADDRESS": 0x30, "BALANCE": 0x31, "ORIGIN": 0x32, "CALLER": 0x33,
    "CALLVALUE": 0x34, "CALLDATALOAD": 0x35, "CALLDATASIZE": 0x36, "CALLDATACOPY": 0x37,
    "CODESIZE": 0x38, "CODECOPY": 0x39, "GASPRICE": 0x3A, "EXTCODESIZE": 0x3B, "EXTCODECOPY": 0x3C,
    "RETURNDATASIZE": 0x3D, "RETURNDATACOPY": 0x3E, "EXTCODEHASH": 0x3F, "BLOCKHASH": 0x40,
    "COINBASE": 0x41, "TIMESTAMP": 0x42, "NUMBER": 0x43, "PREVRANDAO": 0x44, "GASLIMIT": 0x45,
    "CHAINID": 0x46, "SELFBALANCE": 0x47, "BASEFEE": 0x48, "BLOBHASH": 0x49, "BLOBBASEFEE": 0x4A,
    "POP": 0x50, "MLOAD": 0x51, "MSTORE": 0x52, "MSTORE8": 0x53, "SLOAD": 0x54, "SSTORE": 0x55,
    "JUMP": 0x56, "JUMPI": 0x57, "PC": 0x58, "MSIZE": 0x59, "GAS": 0x5A, "JUMPDEST": 0x5B,
    "TLOAD": 0x5C, "TSTORE": 0x5D, "MCOPY": 0x5E, "PUSH0": 0x5F, "LOG0": 0xA0, "LOG1": 0xA1,
    "LOG2": 0xA2, "LOG3": 0xA3, "LOG4": 0xA4, "CREATE": 0xF0, "CALL": 0xF1, "CALLCODE": 0xF2,
    "RETURN": 0xF3, "DELEGATECALL": 0xF4, "CREATE2": 0xF5, "STATICCALL": 0xFA, "REVERT": 0xFD,
    "INVALID": 0xFE, "SELFDESTRUCT": 0xFF,
}
OPS.update({f"DUP{n}": 0x7F + n for n in range(1, 17)})
OPS.update({f"SWAP{n}": 0x8F + n for n in range(1, 17)})


def asm(*lines):
    """Assembles opcodes, `PUSH <value>` (smallest width) and `PUSHn <value>`.

    A `name:` line is a label and `@name` as a push value is the label's offset as a PUSH2.
    Items may be lists of lines or raw bytes.
    """
    items = []
    for line in lines:
        items.extend(line if isinstance(line, list) else [line])
    out, fixups, labels = bytearray(), [], {}
    for item in items:
        if isinstance(item, (bytes, bytearray)):
            out += item
            continue
        parts = item.split()
        if len(parts) == 1 and parts[0].endswith(":"):
            labels[parts[0][:-1]] = len(out)
            out.append(OPS["JUMPDEST"])
            continue
        name = parts[0].upper()
        if name == "PUSH" or re.fullmatch(r"PUSH([1-9]|[12][0-9]|3[0-2])", name):
            value = parts[1]
            if value.startswith("@"):
                out += bytes([0x61, 0, 0])
                fixups.append((len(out) - 2, value[1:]))
                continue
            number = int(value, 0)
            raw = number.to_bytes(max(1, (number.bit_length() + 7) // 8), "big")
            width = len(raw) if name == "PUSH" else int(name[4:])
            out.append(0x5F + width)
            out += raw.rjust(width, b"\0")
        else:
            out.append(OPS[name])
    for at, label in fixups:
        out[at : at + 2] = labels[label].to_bytes(2, "big")
    return bytes(out)


def prefix_gas(code):
    """Gas of the PUSH0 and PUSH opcodes that open `code`, to size a gas limit that runs out at the next opcode."""
    gas = i = 0
    while i < len(code) and 0x5F <= code[i] <= 0x7F:
        gas, i = gas + (2 if code[i] == 0x5F else 3), i + max(code[i] - 0x5E, 1)
    return gas


def hx(data):
    return "0x" + data.hex()


def word(value):
    return value.to_bytes(32, "big")


def rlp_int(value):
    raw = value.to_bytes((value.bit_length() + 7) // 8, "big")
    return rlp_bytes(raw)


def rlp_bytes(raw):
    if len(raw) == 1 and raw[0] < 0x80:
        return raw
    return rlp_len(len(raw), 0x80) + raw


def rlp_list(*items):
    body = b"".join(items)
    return rlp_len(len(body), 0xC0) + body


def rlp_len(length, offset):
    if length < 56:
        return bytes([offset + length])
    raw = length.to_bytes((length.bit_length() + 7) // 8, "big")
    return bytes([offset + 55 + len(raw)]) + raw


def create_address(sender, nonce):
    return "0x" + keccak(rlp_list(rlp_bytes(bytes.fromhex(sender[2:])), rlp_int(nonce)))[12:].hex()


def create2_address(sender, salt, init):
    data = b"\xff" + bytes.fromhex(sender[2:]) + word(salt) + keccak(init)
    return "0x" + keccak(data)[12:].hex()


def initcode(runtime):
    """Init code that deploys `runtime`."""
    n = len(runtime)
    head = asm(f"PUSH {n}", "PUSH 10", "PUSH0", "CODECOPY", f"PUSH {n}", "PUSH0", "RETURN")
    assert len(head) == 10, len(head)
    return head + runtime


def acct(code=b"", balance=0, nonce=0, storage=None):
    return {
        "balance": hex(balance),
        "code": hx(code),
        "nonce": hex(nonce),
        "storage": {f"0x{key:064x}": f"0x{value:064x}" for key, value in (storage or {}).items()},
    }


def call_args(op, to, gas="GAS", value=0, args=(0, 0), ret=(0, 0)):
    """Opcodes for CALL/CALLCODE/DELEGATECALL/STATICCALL, leaving the result on the stack."""
    lines = [f"PUSH {ret[1]}", f"PUSH {ret[0]}", f"PUSH {args[1]}", f"PUSH {args[0]}"]
    if op in ("CALL", "CALLCODE"):
        lines.append(f"PUSH {value}")
    lines.append(f"PUSH20 {to}")
    lines.append(gas if gas == "GAS" else f"PUSH {gas}")
    lines.append(op)
    return lines


def mstore_data(data, base=0):
    """Opcodes that store `data` in memory from `base`."""
    chunks = [data[i : i + 32].ljust(32, b"\0") for i in range(0, len(data), 32)]
    return [x for n, chunk in enumerate(chunks) for x in (f"PUSH32 0x{chunk.hex()}", f"PUSH {base + 32 * n}", "MSTORE")]


def with_data(program, data):
    """Assembles `program(offset)` followed by `data`, where `offset` is where `data` starts."""
    offset = len(asm(program(0)))
    return asm(program(offset)) + data


class Case:
    def __init__(self, name, description, forks=FORKS, tracers=None, node=True):
        self.name, self.description, self.forks = name, description, forks
        self.tracers, self.node = tracers, node
        self.alloc = {SENDER: acct(balance=100 * ETHER)}
        self.txs = []
        self.env = {
            "currentCoinbase": COINBASE,
            "currentDifficulty": "0x0",
            "currentRandom": "0x" + "00" * 30 + "0200",
            "currentGasLimit": "0x1c9c380",
            "currentNumber": "0x1",
            "currentTimestamp": "0x3e8",
            "currentBaseFee": "0x7",
            "currentExcessBlobGas": "0x0",
            "withdrawals": [],
            "parentBeaconBlockRoot": "0x" + "00" * 32,
        }
        self.nonce = 0

    def account(self, address, code=b"", balance=0, nonce=0, storage=None):
        self.alloc[address] = acct(code, balance, nonce, storage)

    def tx(self, to=None, data=b"", gas=1_000_000, value=0, **extra):
        tx = {
            "nonce": hex(extra.pop("nonce", self.nonce)),
            "gasPrice": "0x600",
            "gas": hex(gas),
            "input": hx(data),
            "value": hex(value),
            "v": "0x0",
            "r": "0x0",
            "s": "0x0",
            "secretKey": extra.pop("key", SENDER_KEY),
        }
        if to is not None:
            tx["to"] = to
        tx.update(extra)
        if "maxFeePerGas" in tx:
            del tx["gasPrice"]
        self.nonce += 1
        self.txs.append(tx)
        return tx

    def write(self):
        out = CASES / self.name
        out.mkdir(parents=True, exist_ok=True)
        meta = {"description": self.description, "forks": self.forks}
        if self.tracers is not None:
            meta["tracers"] = self.tracers
        if not self.node:
            meta["node"] = False
        for name, value in (("case", meta), ("alloc", self.alloc), ("env", self.env), ("txs", self.txs)):
            (out / f"{name}.json").write_text(json.dumps(value, indent=2) + "\n")


ALL = []


def case(name, description, **kwargs):
    c = Case(name, description, **kwargs)
    ALL.append(c)
    return c


RUNTIME_42 = asm("PUSH 0x2a", "PUSH0", "MSTORE", "PUSH 0x20", "PUSH0", "RETURN")
REVERT_REASON = asm(  # revert(Error("no"))
    "PUSH 0x08c379a0", "PUSH 0xe0", "SHL", "PUSH0", "MSTORE",
    "PUSH 0x20", "PUSH 4", "MSTORE", "PUSH 2", "PUSH 0x24", "MSTORE",
    "PUSH 0x6e6f", "PUSH 0xf0", "SHL", "PUSH 0x44", "MSTORE", "PUSH 0x64", "PUSH0", "REVERT",
)
STOP = asm("STOP")


def build():
    c = case("plain_opcodes", "Arithmetic, comparison, bitwise, hashing, stack and flow opcodes.")
    code = asm(
        "PUSH 7", "PUSH 5", "ADD", "PUSH 3", "MUL", "PUSH 4", "SWAP1", "SUB", "PUSH 3", "SWAP1", "DIV",
        "PUSH 9", "PUSH 4", "SMOD", "PUSH 5", "PUSH 7", "PUSH 11", "ADDMOD", "PUSH 3", "PUSH 5", "PUSH 7", "MULMOD",
        "PUSH 2", "PUSH 10", "EXP", "PUSH 0xff", "PUSH0", "SIGNEXTEND", "PUSH 1", "PUSH 2", "LT", "PUSH 1", "PUSH 2", "SGT",
        "PUSH 5", "PUSH 5", "EQ", "ISZERO", "PUSH 0xf0", "PUSH 0x0f", "AND", "PUSH 0xf0", "OR", "PUSH 0xff", "XOR", "NOT",
        "PUSH 31", "PUSH 0xabcdef", "BYTE", "PUSH 4", "PUSH 1", "SHL", "PUSH 1", "PUSH 8", "SHR", "PUSH 1", "PUSH 0x80", "SAR",
        "PUSH 0x20", "PUSH0", "KECCAK256", "PC", "GAS", "MSIZE", "CODESIZE", "CALLDATASIZE", "CALLVALUE",
        "DUP1", "DUP2", "DUP3", "SWAP2", "SWAP3", "POP", "POP", "POP",
        "PUSH 1", "PUSH @skip", "JUMPI", "INVALID", "skip:", "PUSH0", "PUSH @end", "JUMP", "INVALID", "end:",
        "PUSH 0x2a", "PUSH0", "MSTORE", "PUSH 0x20", "PUSH0", "RETURN",
    )
    c.account(A1, code)
    c.tx(A1, data=bytes(range(1, 40)), value=5)

    c = case("memory_growth", "Memory expansion through MSTORE, MSTORE8, MLOAD, MCOPY, CALLDATACOPY, KECCAK256 and RETURN.")
    code = asm(
        "PUSH 0x11", "PUSH0", "MSTORE", "PUSH 0x22", "PUSH 0x20", "MSTORE", "PUSH 0x33", "PUSH 0x3f", "MSTORE8",
        "PUSH 0x100", "MLOAD", "POP", "MSIZE", "POP", "PUSH 0x40", "PUSH0", "PUSH 0x200", "MCOPY",
        "PUSH 0x30", "PUSH0", "PUSH 0x300", "CALLDATACOPY", "PUSH 0x40", "PUSH 0x2f0", "KECCAK256", "POP",
        "PUSH 0x20", "PUSH 0x410", "RETURN",
    )
    c.account(A1, code)
    c.tx(A1, data=bytes(range(0x30)))

    c = case("sstore_sload_refund", "Cold and warm SLOAD and SSTORE: set, update, clear and restore with refunds.")
    code = asm(
        "PUSH 1", "PUSH 1", "SSTORE", "PUSH 1", "SLOAD", "POP", "PUSH 2", "PUSH 1", "SSTORE", "PUSH 0", "PUSH 1", "SSTORE",
        "PUSH 3", "PUSH 2", "SSTORE", "PUSH 3", "SLOAD", "POP", "PUSH 0", "PUSH 3", "SSTORE",
        "PUSH 9", "PUSH 4", "SSTORE", "PUSH 0", "PUSH 4", "SSTORE", "PUSH 0", "PUSH 5", "SSTORE", "STOP",
    )
    c.account(A1, code, storage={2: 7, 3: 8, 5: 6})
    c.tx(A1)

    c = case("sstore_refund_cap", "SSTORE clears refund more than a fifth of the gas used.")
    clears = [x for slot in range(1, 9) for x in ("PUSH 0", f"PUSH {slot}", "SSTORE")]
    c.account(A1, asm(*clears, "STOP"), storage={slot: 1 for slot in range(1, 9)})
    c.tx(A1)

    c = case("sstore_stipend_oog", "SSTORE with 2300 gas or less left fails the reentrancy sentry.")
    code = asm("PUSH 1", "PUSH 0", "SSTORE", "STOP")
    c.account(A1, code, storage={0: 1})
    c.tx(A1, gas=21000 + prefix_gas(code) + 2300)

    c = case("transient_storage", "TSTORE and TLOAD, including across a call.")
    inner = asm("PUSH 0", "TLOAD", "PUSH0", "MSTORE", "PUSH 0x20", "PUSH0", "RETURN")
    c.account(A2, inner)
    c.account(A1, asm("PUSH 0x77", "PUSH 0", "TSTORE", "PUSH 0", "TLOAD", "POP", call_args("CALL", A2, ret=(0, 32)), "POP", "PUSH0", "MLOAD", "POP", "STOP"))
    c.tx(A1)

    c = case("log_ops", "LOG0 to LOG4 with data and memory expansion.")
    code = asm(
        "PUSH 0xdeadbeef", "PUSH0", "MSTORE", "PUSH 0x20", "PUSH0", "LOG0",
        "PUSH 0x11", "PUSH 0x20", "PUSH 0x10", "LOG1", "PUSH 0x22", "PUSH 0x11", "PUSH 0x40", "PUSH 0x20", "LOG2",
        "PUSH 0x33", "PUSH 0x22", "PUSH 0x11", "PUSH 0", "PUSH 0x20", "LOG3",
        "PUSH 0x44", "PUSH 0x33", "PUSH 0x22", "PUSH 0x11", "PUSH 0x60", "PUSH 0x40", "LOG4", "STOP",
    )
    c.account(A1, code)
    c.tx(A1)

    c = case("log_in_reverted_call", "A log emitted by a call that later reverts disappears.")
    c.account(A2, asm("PUSH 1", "PUSH0", "PUSH0", "LOG1", "PUSH0", "PUSH0", "REVERT"))
    c.account(A1, asm(call_args("CALL", A2), "POP", "PUSH 2", "PUSH0", "PUSH0", "LOG1", "STOP"))
    c.tx(A1)

    c = case("env_opcodes", "Account and block environment opcodes.", node=False)
    c.env["blockHashes"] = {"0": "0x" + "ab" * 32}
    ext = asm("PUSH 0x42", "PUSH0", "MSTORE", "PUSH 0x20", "PUSH0", "RETURN")
    c.account(A2, ext, balance=77)
    code = asm(
        "ADDRESS", "BALANCE", "ORIGIN", "CALLER", "CALLVALUE", "GASPRICE", f"PUSH20 {A2}", "EXTCODESIZE",
        f"PUSH20 {A2}", "EXTCODEHASH", "PUSH 8", "PUSH0", "PUSH0", f"PUSH20 {A2}", "EXTCODECOPY", "PUSH0", "BLOCKHASH",
        "COINBASE", "TIMESTAMP", "NUMBER", "PREVRANDAO", "GASLIMIT", "CHAINID", "SELFBALANCE", "BASEFEE",
        "PUSH0", "BLOBHASH", "BLOBBASEFEE", "STOP",
    )
    c.account(A1, code, balance=5)
    c.tx(A1, value=3)

    c = case("clz", "CLZ opcode.", forks=["Osaka", "Amsterdam"])
    c.account(A1, asm("PUSH 1", "CLZ", "PUSH0", "CLZ", "PUSH 0x80", "PUSH 248", "SHL", "CLZ", "STOP"))
    c.tx(A1)

    # --- calls
    for op in ("CALL", "CALLCODE", "DELEGATECALL", "STATICCALL"):
        c = case(f"{op.lower()}_basic", f"{op} into a contract that returns data and writes storage.")
        write = [] if op == "STATICCALL" else ["PUSH 5", "PUSH 1", "SSTORE"]
        c.account(A2, asm("CALLER", "PUSH0", "MSTORE", "CALLVALUE", "PUSH 0x20", "MSTORE", write, "PUSH 0x40", "PUSH0", "RETURN"))
        value = 3 if op in ("CALL", "CALLCODE") else 0
        c.account(A1, asm(call_args(op, A2, value=value, ret=(0, 0x40)), "PUSH 0", "SSTORE", "RETURNDATASIZE", "PUSH 1", "SSTORE", "PUSH 0x40", "PUSH0", "RETURN"), balance=10)
        c.tx(A1, value=2)

    c = case("call_stipend", "CALL with value gives the callee a 2300 gas stipend, which pays for a LOG but not for SSTORE.")
    c.account(A2, asm("PUSH 1", "PUSH0", "PUSH0", "LOG1", "STOP"))
    c.account(A3, asm("PUSH 1", "PUSH0", "SSTORE", "STOP"))
    c.account(A1, asm(call_args("CALL", A2, gas=0, value=1), "PUSH 0", "SSTORE", call_args("CALL", A3, gas=0, value=1), "PUSH 1", "SSTORE", "STOP"), balance=10)
    c.tx(A1)

    c = case("call_new_account", "CALL with value to a nonexistent account pays the new account cost; zero value does not.")
    c.account(A1, asm(call_args("CALL", EMPTY, value=1), "PUSH 0", "SSTORE", call_args("CALL", "0x" + "00" * 19 + "98", value=0), "PUSH 1", "SSTORE", "STOP"), balance=10)
    c.tx(A1)

    c = case("call_revert_reason", "A callee reverts with an Error(string) reason; the caller copies the return data.")
    c.account(A2, REVERT_REASON)
    c.account(A1, asm(call_args("CALL", A2, ret=(0, 0x20)), "PUSH0", "SSTORE", "RETURNDATASIZE", "PUSH0", "PUSH0", "RETURNDATACOPY", "RETURNDATASIZE", "PUSH0", "REVERT"))
    c.tx(A1)

    c = case("call_returndata", "RETURNDATASIZE and RETURNDATACOPY after calls with different return sizes.")
    c.account(A2, RUNTIME_42)
    c.account(A3, asm("PUSH 0xff", "PUSH 0x30", "MSTORE", "PUSH 0x50", "PUSH0", "RETURN"))
    c.account(A1, asm(call_args("CALL", A2), "POP", "RETURNDATASIZE", "PUSH0", "PUSH0", "RETURNDATACOPY", call_args("STATICCALL", A3, ret=(0x100, 0x10)), "POP", "RETURNDATASIZE", "PUSH 0x20", "PUSH 0x10", "RETURNDATACOPY", "PUSH 0x80", "PUSH0", "RETURN"))
    c.tx(A1)

    c = case("call_nested", "Three nested calls, a revert in the middle frame and logs at every depth.")
    c.account(A3, asm("PUSH 3", "PUSH0", "PUSH0", "LOG1", "PUSH 1", "PUSH 1", "SSTORE", "PUSH0", "PUSH0", "RETURN"))
    c.account(A2, asm("PUSH 2", "PUSH0", "PUSH0", "LOG1", call_args("CALL", A3), "POP", "PUSH0", "PUSH0", "REVERT"))
    c.account(A1, asm("PUSH 1", "PUSH0", "PUSH0", "LOG1", call_args("CALL", A2), "PUSH0", "SSTORE", call_args("DELEGATECALL", A3), "PUSH 1", "SSTORE", "STOP"))
    c.tx(A1)

    c = case("call_insufficient_balance", "CALL and CREATE with more value than the contract holds fail without a frame.")
    c.account(A2, STOP)
    c.account(A1, asm(call_args("CALL", A2, value=1000), "PUSH0", "SSTORE", "PUSH 0", "PUSH0", "PUSH 1000", "CREATE", "PUSH 1", "SSTORE", "STOP"), balance=10)
    c.tx(A1)

    c = case("call_depth_limit", "A contract calls itself until the depth limit stops it.", forks=["Cancun", "Prague"],
             tracers=["call", "call-top", "4byte", "noop"], node=False)
    c.account(A1, asm("PUSH0", "PUSH0", "PUSH0", "PUSH0", "PUSH0", "ADDRESS", "GAS", "CALL", "STOP"))
    c.env["currentGasLimit"] = hex(10**13)
    c.tx(A1, gas=5_000_000_000_000)

    c = case("call_revert_gas_accounting", "Callee consumes all its gas in an invalid opcode while the caller keeps 1/64.")
    c.account(A2, asm("INVALID"))
    c.account(A1, asm(call_args("CALL", A2), "PUSH0", "SSTORE", "GAS", "PUSH 1", "SSTORE", "STOP"))
    c.tx(A1)

    # --- precompiles
    key = keys.PrivateKey(bytes.fromhex("11" * 32))
    digest = keccak(b"evm2 parity")
    sig = key.sign_msg_hash(digest)
    ecrecover_input = digest + word(sig.v + 27) + word(sig.r) + word(sig.s)
    blake2f = (12).to_bytes(4, "big") + bytes(64) + b"abc".ljust(128, b"\0") + (3).to_bytes(8, "little") + bytes(8) + b"\x01"
    def call_precompile(name, number, data, description):
        c = case(f"precompile_{name}", description)
        lines = mstore_data(data)
        lines += call_args("STATICCALL", f"0x{number:040x}", gas=100000, args=(0, len(data)), ret=(0x100, 0x80))
        lines += ["PUSH0", "SSTORE", "PUSH 0x80", "PUSH 0x100", "RETURN"]
        c.account(A1, asm(*lines))
        c.tx(A1, gas=500_000)

    for name, number, data in (
        ("ecrecover", 1, ecrecover_input), ("sha256", 2, b"hello"), ("ripemd160", 3, b"hello"), ("identity", 4, b"hello"),
        ("modexp", 5, word(1) + word(1) + word(1) + b"\x03\x02\x05"),
        ("bn_add", 6, word(1) + word(2) + word(1) + word(2)), ("bn_mul", 7, word(1) + word(2) + word(3)),
        ("bn_pairing", 8, b""), ("blake2f", 9, blake2f),
    ):
        call_precompile(name, number, data, f"The {name} precompile called with valid input.")

    c = case("precompile_point_evaluation", "The KZG point evaluation precompile rejects malformed input.")
    c.account(A1, asm("PUSH 192", "PUSH0", "PUSH 192", "PUSH0", "PUSH 10", "GAS", "STATICCALL", "PUSH0", "SSTORE", "STOP"))
    c.tx(A1, gas=500_000)

    c = case("precompile_bls_p256", "BLS12-381 G1 addition and P-256 verification with trivial input.", forks=PRAGUE)
    c.account(A1, asm(
        call_args("STATICCALL", f"0x{0x0B:040x}", gas=100000, args=(0, 256), ret=(0x400, 128)), "PUSH0", "SSTORE",
        call_args("STATICCALL", f"0x{0x100:040x}", gas=100000, args=(0, 160), ret=(0x500, 32)), "PUSH 1", "SSTORE", "STOP",
    ))
    c.tx(A1, gas=500_000)

    c = case("precompile_failing", "Precompile calls that fail: bad blake2f input, oversized modexp, short pairing input, too little gas.")
    lines = []
    bad = [(9, b"short"), (5, word(0x10000) * 3), (8, b"\x01" * 100), (1, b"")]
    for index, (number, data) in enumerate(bad):
        address = f"0x{number:040x}"
        lines += mstore_data(data, index * 0x80)
        lines += call_args("CALL", address, gas=50000, args=(index * 0x80, len(data)), ret=(0x200, 0x20))
        lines += [f"PUSH {index}", "SSTORE"]
    lines += call_args("CALL", f"0x{2:040x}", gas=10, args=(0, 32), ret=(0, 32)) + ["PUSH 9", "SSTORE", "STOP"]
    c.account(A1, asm(*lines))
    c.tx(A1, gas=2_000_000)

    c = case("precompile_top_level", "A transaction sent straight to a precompile.")
    c.tx(f"0x{4:040x}", data=b"abc", value=1)
    c.tx(f"0x{2:040x}", data=b"abc")

    # --- creation
    init = initcode(RUNTIME_42)

    def factory(op, init_code, value=0, salt=1):
        n = len(init_code)

        def program(offset):
            lines = [f"PUSH {n}", f"PUSH2 {offset}", "PUSH0", "CODECOPY"]
            if op == "CREATE2":
                lines.append(f"PUSH {salt}")
            return [*lines, f"PUSH {n}", "PUSH0", f"PUSH {value}", op, "PUSH0", "SSTORE", "STOP"]

        return with_data(program, init_code)

    c = case("create_success", "CREATE and CREATE2 deploy a contract; the factory also keeps the value.")
    c.account(A1, factory("CREATE", init, value=1), balance=5)
    c.account(A2, factory("CREATE2", init, value=1), balance=5)
    c.account(A3, asm(call_args("CALL", A1), "POP", call_args("CALL", A2), "POP", "STOP"))
    c.tx(A3)

    c = case("create_tx", "Contract creation transactions, one that deploys and one whose init code reverts.")
    c.tx(None, data=init, value=1)
    c.tx(None, data=asm("PUSH0", "PUSH0", "REVERT"))
    c.tx(None, data=asm("INVALID"))

    c = case("create_init_revert", "Init code reverts with a reason and with no data.")
    c.account(A1, factory("CREATE", REVERT_REASON), balance=5)
    c.account(A2, factory("CREATE2", asm("PUSH0", "PUSH0", "REVERT")), balance=5)
    c.account(A3, asm(call_args("CALL", A1), "POP", call_args("CALL", A2), "POP", "STOP"))
    c.tx(A3)

    c = case("create_init_oog", "Init code runs out of gas inside CREATE and CREATE2.")
    loop = asm("PUSH 1", "PUSH 0x1000000", "MSTORE")
    c.account(A1, factory("CREATE", loop))
    c.account(A2, factory("CREATE2", loop))
    c.account(A3, asm(call_args("CALL", A1, gas=100000), "POP", call_args("CALL", A2, gas=100000), "POP", "STOP"))
    c.tx(A3, gas=1_000_000)

    c = case("create2_collision", "CREATE2 to an address that already has code, or a nonce, fails.")
    c.account(A1, factory("CREATE2", init, salt=1), balance=5)
    c.account(create2_address(A1, 1, init), STOP)
    c.tx(A1)
    c2 = case("create_collision_nonce", "CREATE to an address with a nonzero nonce fails.")
    c2.account(A1, factory("CREATE", init), balance=5, nonce=1)
    c2.account(create_address(A1, 1), b"", nonce=1)
    c2.tx(A1)

    c = case("create_nonce_overflow", "CREATE and CREATE2 from a creator whose nonce is 2**64-1 fail without a frame result.")
    max_nonce = 2**64 - 1
    c.account(A1, factory("CREATE", init, value=1), balance=5, nonce=max_nonce)
    c.account(A2, factory("CREATE2", init, value=1), balance=5, nonce=max_nonce)
    c.account(A3, asm(call_args("CALL", A1), "POP", call_args("CALL", A2), "POP", "STOP"))
    c.tx(A3)

    c = case("create_code_rules", "Deployed code that starts with 0xef or exceeds the code size limit.")
    c.account(A1, factory("CREATE", asm("PUSH 0xef", "PUSH0", "MSTORE8", "PUSH 1", "PUSH0", "RETURN")))
    c.account(A2, factory("CREATE", asm("PUSH 0x6001", "PUSH0", "RETURN")))
    c.account(A3, asm(call_args("CALL", A1), "POP", call_args("CALL", A2), "POP", "STOP"))
    c.tx(A3, gas=3_000_000)

    c = case("create_selfdestruct_in_init", "Init code that selfdestructs deploys nothing.")
    c.tx(None, data=asm(f"PUSH20 {A5}", "SELFDESTRUCT"), value=3)

    # --- out of gas
    def oog(name, description, lines, remaining):
        c = case(name, description)
        code = asm(*lines)
        c.account(A1, code, storage={9: 1}, balance=1)
        c.account(A2, STOP, balance=1)
        c.tx(A1, gas=21000 + prefix_gas(code) + remaining)

    oog("oog_cold_balance", "Out of gas at a cold BALANCE.", [f"PUSH20 {A2}", "BALANCE", "STOP"], 642)
    oog("oog_cold_sload", "Out of gas at a cold SLOAD.", ["PUSH 9", "SLOAD", "STOP"], 600)
    oog("oog_cold_call", "Out of gas at a cold CALL.", call_args("CALL", A2, gas=0) + ["STOP"], 50)
    oog("oog_value_call", "Out of gas at a CALL with value.", call_args("CALL", A2, gas=0, value=1) + ["STOP"], 5000)
    oog("oog_cold_sstore", "Out of gas at a cold SSTORE.", ["PUSH 1", "PUSH 7", "SSTORE", "STOP"], 5000)
    oog("oog_push1", "Out of gas at PUSH1 with 2 gas left.", ["PUSH 1", "STOP"], 2)
    oog("oog_huge_mstore", "Out of gas at an MSTORE far beyond the memory limit.", ["PUSH 1", "PUSH 0x1000000", "MSTORE", "STOP"], 3_000_003)
    oog("oog_log0_memory", "Out of gas at LOG0 that expands memory.", ["PUSH 0x20", "PUSH0", "LOG0", "STOP"], 385)
    oog("oog_cold_call_forward", "Out of gas at a cold CALL that forwards 1024 gas.", call_args("CALL", A2, gas=1024) + ["STOP"], 3000)
    oog("oog_extcodesize", "Out of gas at a cold EXTCODESIZE.", [f"PUSH20 {A2}", "EXTCODESIZE", "STOP"], 1000)
    oog("oog_sha3_memory", "Out of gas at KECCAK256 over fresh memory.", ["PUSH 0x400", "PUSH0", "KECCAK256", "STOP"], 100)
    oog("oog_exp", "Out of gas at EXP with a large exponent.", ["PUSH 0xffffffff", "PUSH 2", "EXP", "STOP"], 100)
    oog("oog_selfdestruct", "Out of gas at SELFDESTRUCT to a cold new account.", [f"PUSH20 {EMPTY}", "SELFDESTRUCT"], 2000)
    oog("oog_create", "Out of gas at CREATE with init code memory.", ["PUSH 0x100", "PUSH0", "PUSH0", "CREATE", "STOP"], 31000)

    c = case("oog_in_callee", "A callee runs out of gas at a cold SLOAD while the caller continues.")
    callee = asm("PUSH 9", "SLOAD", "STOP")
    c.account(A2, callee, storage={9: 1})
    c.account(A1, asm(call_args("CALL", A2, gas=800), "PUSH0", "SSTORE", "STOP"))
    c.tx(A1)

    c = case("oog_in_nested_create", "A callee deploys with init code that runs out of gas.")
    c.account(A2, factory("CREATE", loop))
    c.account(A1, asm(call_args("CALL", A2, gas=30000), "PUSH0", "SSTORE", "STOP"))
    c.tx(A1)

    # --- exceptional halts
    def halt(name, description, code, **kw):
        c = case(name, description, **kw)
        c.account(A2, code)
        c.account(A1, asm(call_args("CALL", A2), "PUSH0", "SSTORE", "STOP"))
        c.account(A3, code)
        c.tx(A3)
        c.tx(A1)

    halt("stack_underflow", "ADD with an empty stack, at the top level and in a callee.", asm("ADD"))
    halt("invalid_opcode", "INVALID and an undefined opcode.", asm("PUSH 1", "INVALID"))
    halt("undefined_opcode", "The undefined opcode 0x0c.", bytes([0x60, 1, 0x0C]))
    halt("invalid_jump", "A JUMP to a position that is not a JUMPDEST.", asm("PUSH 5", "JUMP", "STOP"))
    halt("invalid_jump_in_push", "A JUMP into push data that looks like a JUMPDEST.", bytes([0x60, 0x04, 0x56, 0x60, 0x5B, 0x00]))
    halt("stack_overflow", "Pushing past 1024 stack items.", asm(*(["PUSH0"] * 1025)), tracers=["call", "call-top", "flat", "4byte", "noop", "prestate"])
    halt("returndata_oob", "RETURNDATACOPY past the end of the return data.", asm("PUSH 1", "PUSH0", "PUSH0", "RETURNDATACOPY"))
    halt("revert_empty", "REVERT with no data.", asm("PUSH0", "PUSH0", "REVERT"))
    halt("revert_reason", "REVERT with an Error(string) reason.", REVERT_REASON)

    c = case("static_call_violation", "State changing opcodes inside a STATICCALL.")
    violations = {
        A2: asm("PUSH 1", "PUSH0", "SSTORE"),
        A3: asm("PUSH0", "PUSH0", "LOG0"),
        A4: asm("PUSH0", "PUSH0", "PUSH0", "CREATE"),
        A5: asm(f"PUSH20 {A1}", "SELFDESTRUCT"),
        "0x" + "00" * 19 + "a1": asm(call_args("CALL", EMPTY, value=1)),
        "0x" + "00" * 19 + "a2": asm("PUSH 1", "PUSH0", "TSTORE"),
    }
    lines = []
    for index, (address, code) in enumerate(violations.items()):
        c.account(address, code, balance=1)
        lines += [call_args("STATICCALL", address), f"PUSH {index}", "SSTORE"]
    c.account(A1, asm(*lines, "STOP"), balance=1)
    c.tx(A1)

    c = case("static_log_memory", "LOG0 and LOG2 with data inside a STATICCALL fail after growing the memory.")
    c.account(A2, asm("PUSH 0x20", "PUSH 0x40", "LOG0"))
    c.account(A3, asm("PUSH 1", "PUSH 2", "PUSH 0x10", "PUSH 0x60", "LOG2"))
    c.account(A1, asm(call_args("STATICCALL", A2), "POP", call_args("STATICCALL", A3), "POP", "STOP"))
    c.tx(A1)

    # --- selfdestruct
    c = case("selfdestruct_existing", "SELFDESTRUCT of a contract that existed before the transaction.")
    c.account(A2, asm(f"PUSH20 {A4}", "SELFDESTRUCT"), balance=9, storage={1: 1})
    c.account(A1, asm(call_args("CALL", A2), "POP", "STOP"))
    c.tx(A1)

    c = case("selfdestruct_same_tx", "SELFDESTRUCT of a contract created in the same transaction.")
    sd_init = initcode(asm(f"PUSH20 {A4}", "SELFDESTRUCT"))

    def create_and_call(offset):
        return [f"PUSH {len(sd_init)}", f"PUSH2 {offset}", "PUSH0", "CODECOPY", f"PUSH {len(sd_init)}", "PUSH0", "PUSH 7", "CREATE", "DUP1", "PUSH0", "SSTORE",
                "PUSH0", "PUSH0", "PUSH0", "PUSH0", "PUSH0", "DUP6", "GAS", "CALL", "POP", "STOP"]

    c.account(A1, with_data(create_and_call, sd_init), balance=10)
    c.tx(A1)

    c = case("selfdestruct_to_self", "SELFDESTRUCT with the contract itself as the beneficiary.")
    c.account(A2, asm("ADDRESS", "SELFDESTRUCT"), balance=9)
    c.account(A1, asm(call_args("CALL", A2), "POP", "STOP"))
    c.tx(A1)

    c = case("selfdestruct_new_account", "SELFDESTRUCT with value to a new account, and a repeat in the same call tree.")
    c.account(A2, asm(f"PUSH20 {EMPTY}", "SELFDESTRUCT"), balance=9)
    c.account(A1, asm(call_args("CALL", A2), "PUSH0", "SSTORE", call_args("CALL", A2), "PUSH 1", "SSTORE", "STOP"))
    c.tx(A1)

    # --- transactions
    c = case("tx_transfers", "Value transfers to an EOA, a new account, a contract without code and with zero value.")
    c.account(A1, STOP)
    c.tx(A5, value=1)
    c.tx(EMPTY, value=2, gas=21000)
    c.tx(A1, value=3, gas=21000)
    c.tx(A5, value=0, gas=21000)
    c.tx(A5, data=b"\x00\x01\x00\x02", gas=21040)

    c = case("tx_rejected", "Transactions that are not valid: wrong nonce, no funds, gas below intrinsic, gas above the block limit.")
    c.tx(A5, value=1)
    c.tx(A5, value=1, nonce=5)
    c.tx(A5, value=1, nonce=0)
    c.tx(A5, value=1, gas=20999, nonce=1)
    c.tx(A5, value=ETHER * 1000, nonce=1)
    c.tx(A5, gas=0x2000000, nonce=1)
    c.tx(A5, gas=21000, nonce=1, gasPrice="0x1")

    c = case("tx_rejected_underfunded_value", "A sender that can pay for gas but not for the value: the create and the call are rejected, a smaller create after them runs.", node=False)
    poor_key = keys.PrivateKey(bytes([0x42]) * 32)
    poor = poor_key.public_key.to_checksum_address().lower()
    c.account(poor, balance=300_000 * 0x600 + 1000)
    secret = "0x" + poor_key.to_bytes().hex()
    c.tx(None, data=initcode(RUNTIME_42), value=2000, gas=300_000, nonce=0, key=secret)
    c.tx(A5, value=2000, gas=300_000, nonce=0, key=secret)
    c.tx(None, data=initcode(RUNTIME_42), value=500, gas=300_000, nonce=0, key=secret)

    c = case("tx_multi", "Several transactions in one block with state dependencies and cumulative gas.")
    c.account(A1, asm("PUSH0", "SLOAD", "PUSH 1", "ADD", "DUP1", "PUSH0", "SSTORE", "PUSH0", "MSTORE", "PUSH 0x20", "PUSH0", "RETURN"))
    c.account(A2, asm("PUSH 1", "PUSH0", "PUSH0", "LOG1", "STOP"))
    for to in (A1, A2, A1, A5):
        c.tx(to, value=1)

    c = case("tx_access_list", "EIP-2930 and EIP-1559 transactions with access lists.")
    c.account(A1, asm("PUSH 1", "SLOAD", "POP", f"PUSH20 {A2}", "BALANCE", "POP", "STOP"), storage={1: 1})
    c.account(A2, STOP, balance=1)
    c.tx(A1, type="0x1", chainId="0x1", accessList=[{"address": A1, "storageKeys": ["0x" + "00" * 31 + "01"]}, {"address": A2, "storageKeys": []}])
    c.tx(A1, type="0x2", chainId="0x1", maxFeePerGas="0x600", maxPriorityFeePerGas="0x100", accessList=[{"address": A1, "storageKeys": []}])

    c = case("tx_blob", "A blob transaction reads BLOBHASH and BLOBBASEFEE.", node=False)
    c.account(A1, asm("PUSH0", "BLOBHASH", "PUSH0", "SSTORE", "BLOBBASEFEE", "PUSH 1", "SSTORE", "PUSH 1", "BLOBHASH", "PUSH 2", "SSTORE", "STOP"))
    c.tx(A1, type="0x3", chainId="0x1", maxFeePerGas="0x600", maxPriorityFeePerGas="0x100", maxFeePerBlobGas="0x10",
         blobVersionedHashes=["0x01" + "ab" * 31], accessList=[])

    # --- block level rules
    def rich_sender(c, n):
        """Adds a funded account that signs with the key made of the byte `n`."""
        key = keys.PrivateKey(bytes([n]) * 32)
        c.account(key.public_key.to_checksum_address().lower(), balance=100 * ETHER)
        return "0x" + key.to_bytes().hex()

    c = case("blob_block_limit", "Blob transactions past the block's blob gas limit are rejected.", node=False)
    for n in range(1, 6):
        c.tx(A5, type="0x3", chainId="0x1", maxFeePerGas="0x600", maxPriorityFeePerGas="0x100", maxFeePerBlobGas="0x10",
             blobVersionedHashes=["0x01" + f"{n:02x}" * 31] * 6, accessList=[], nonce=0, key=rich_sender(c, n))

    c = case("withdrawals", "Withdrawals credit existing and new accounts; an amount of zero must not create an account.")
    c.account(A1, STOP, balance=1)
    c.account(A2, b"", balance=0)
    c.env["withdrawals"] = [
        {"index": hex(i), "validatorIndex": hex(i), "address": address, "amount": hex(amount)}
        for i, (address, amount) in enumerate(((A1, 5), (A2, 0), (A3, 0), (A4, 7), (A4, 1)))
    ]
    c.tx(A5, value=1)

    c = case("deposit_request", "A log of the deposit contract becomes an EIP-6110 request.", forks=PRAGUE)
    deposit = "0x00000000219ab540356cbb839cbe05303d7705fa"
    c.account(deposit, asm("CALLDATASIZE", "PUSH0", "PUSH0", "CALLDATACOPY",
                           "PUSH 0x649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5", "CALLDATASIZE", "PUSH0", "LOG1", "STOP"))
    event = bytearray(576)
    for offset, size, fill in ((192, 48, 0xA1), (288, 32, 0xB2), (352, 8, 0xC3), (416, 96, 0xD4), (544, 8, 0xE5)):
        event[offset : offset + size] = bytes([fill]) * size
    c.tx(deposit, data=bytes(event))

    c = case("system_call_revert", "The beacon roots system call fails and the failure is ignored.")
    c.account("0x000f3df6d732807ef1319fb7b8bb8522d0beac02", asm("PUSH0", "PUSH0", "REVERT"))
    c.env["parentBeaconBlockRoot"] = "0x" + "11" * 32
    c.account(A1, STOP)
    c.tx(A1, value=1)

    # --- EIP-7702
    auth_key = keys.PrivateKey(bytes.fromhex("22" * 32))
    authority = auth_key.public_key.to_checksum_address().lower()

    def authorization(chain_id, address, nonce):
        digest = keccak(b"\x05" + rlp_list(rlp_int(chain_id), rlp_bytes(bytes.fromhex(address[2:])), rlp_int(nonce)))
        sig = auth_key.sign_msg_hash(digest)
        return {"chainId": hex(chain_id), "address": address, "nonce": hex(nonce), "yParity": hex(sig.v), "r": hex(sig.r), "s": hex(sig.s)}

    c = case("eip7702_delegation", "A set-code transaction delegates an EOA to a contract, which a second transaction and a contract call.", forks=PRAGUE)
    c.account(A2, asm("CALLER", "PUSH0", "MSTORE", "ADDRESS", "PUSH 0x20", "MSTORE", "PUSH 1", "PUSH 7", "SSTORE", "PUSH 0x40", "PUSH0", "RETURN"))
    c.account(authority, b"", balance=ETHER)
    c.account(A1, asm(call_args("CALL", authority, ret=(0, 0x40)), "PUSH0", "SSTORE", f"PUSH20 {authority}", "EXTCODESIZE", "PUSH 1", "SSTORE", f"PUSH20 {authority}", "EXTCODEHASH", "PUSH 2", "SSTORE", "STOP"))
    c.tx(A3, type="0x4", chainId="0x1", maxFeePerGas="0x600", maxPriorityFeePerGas="0x100", accessList=[],
         authorizationList=[authorization(1, A2, 0), authorization(1, A2, 7), authorization(2, A2, 0)], gas=200000)
    c.tx(authority, data=b"\x01", gas=200000)
    c.tx(A1, gas=300000)

    c = case("eip7702_clear", "A set-code transaction resets the delegation to the zero address.", forks=PRAGUE)
    c.account(A2, STOP)
    c.account(authority, b"\xef\x01\x00" + bytes.fromhex(A2[2:]), balance=ETHER)
    c.tx(A3, type="0x4", chainId="0x1", maxFeePerGas="0x600", maxPriorityFeePerGas="0x100", accessList=[],
         authorizationList=[authorization(1, ZERO_ADDR, 0)], gas=100000)
    c.tx(authority, gas=100000)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("only", nargs="*", help="substring filter on case names")
    args = parser.parse_args()
    build()
    names = [c.name for c in ALL]
    assert len(names) == len(set(names)), "duplicate case names"
    selected = [c for c in ALL if not args.only or any(o in c.name for o in args.only)]
    if not args.only:
        for stale in CASES.iterdir():
            if stale.is_dir() and stale.name not in names:
                shutil.rmtree(stale)
    for c in selected:
        c.write()
    print(f"wrote {len(selected)} cases to {CASES.relative_to(ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
