#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.13"
# ///
"""Convert evm-protobuf-fuzzer corpus entries into evm2-fuzzer JSON cases.

This is a one-time import helper for the corpus produced by
`rakita/el-fuzzers/evm-protobuf-fuzzer`. It reads either the `.tar.xz` corpus
archive or an extracted corpus directory and writes JSON files that can be
replayed with:

    cargo run -q -p evm2-fuzzer -- corpus tmp/evm-protobuf-import
"""

from __future__ import annotations

import argparse
import json
import shutil
import tarfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable

from utils import repo_root

ROOT = Path(repo_root())
DEFAULT_OUTPUT = ROOT / "tmp" / "evm-protobuf-import"
DEFAULT_SOURCE = (
    ROOT.parent.parent
    / "rakita"
    / "el-fuzzers"
    / "evm-protobuf-fuzzer"
    / "corp-evm-protobuf-fuzzer.tar.xz"
)

MAX_GAS = 35_000_000
MAX_ACCOUNTS = 10
MAX_STORAGE_PER_ACCOUNT = 10
MAX_ACCESS_LIST_PER_TX = 10
MAX_ACCESS_LIST_KEYS_PER_TX = 10
MAX_DATA_SIZE = 30_000
MAX_TXS = 10
MAX_EXCESS_BLOB_GAS = 99_999_999
MAX_NONCE = 18_446_744_073_709_551_600
OSAKA_TX_GAS_LIMIT_CAP = 16_777_216
BENEFICIARY = "0x" + "30" * 20


@dataclass(frozen=True)
class Field:
    number: int
    wire_type: int
    value: int | bytes | None = None


@dataclass
class UInt256:
    v1: int = 0
    v2: int = 0
    v3: int = 0
    v4: int = 0

    def raw_bytes(self) -> bytes:
        return b"".join(
            value.to_bytes(8, "little") for value in (self.v1, self.v2, self.v3, self.v4)
        )

    def word(self) -> int:
        return int.from_bytes(self.raw_bytes(), "big")

    def word_hex(self) -> str:
        return quantity(self.word())

    def b256_hex(self) -> str:
        return "0x" + self.raw_bytes().hex()

    def address(self) -> str:
        return "0x" + self.raw_bytes()[12:].hex()

    def is_special_address(self) -> bool:
        data = self.raw_bytes()
        return all(byte == 0 for byte in data[12:31])


@dataclass
class StorageEntry:
    key: UInt256 = field(default_factory=UInt256)
    value: UInt256 = field(default_factory=UInt256)


@dataclass
class Account:
    address: UInt256 = field(default_factory=UInt256)
    code: bytes = b""
    balance: int = 0
    nonce: int = 0
    storage: list[StorageEntry] = field(default_factory=list)


@dataclass
class Block:
    number: int = 0
    timestamp: int = 0
    gaslimit: int = 0
    coinbase: UInt256 = field(default_factory=UInt256)
    difficulty: UInt256 = field(default_factory=UInt256)
    basefee: int = 0
    excessblobgas: int = 0


@dataclass
class AccessListEntry:
    address: UInt256 = field(default_factory=UInt256)
    keys: list[UInt256] = field(default_factory=list)


@dataclass
class Transaction:
    sender: UInt256 = field(default_factory=UInt256)
    recipient: UInt256 = field(default_factory=UInt256)
    data: bytes = b""
    gas: int = 0
    value: int = 0
    creates: bool = False
    access_list: list[AccessListEntry] = field(default_factory=list)


@dataclass
class Prestate:
    accounts: list[Account] = field(default_factory=list)
    block: Block = field(default_factory=Block)
    txs: list[Transaction] = field(default_factory=list)


@dataclass
class Stats:
    seen: int = 0
    converted: int = 0
    skipped_parse: int = 0
    skipped_validate: int = 0
    skipped_no_txs: int = 0
    skipped_duplicate: int = 0
    overwritten: int = 0


def quantity(value: int) -> str:
    return hex(value)


def bytes_hex(data: bytes) -> str:
    return "0x" + data.hex()


class ProtoError(ValueError):
    pass


class ProtoReader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.index = 0

    def eof(self) -> bool:
        return self.index >= len(self.data)

    def read_varint(self) -> int:
        value = 0
        shift = 0
        while self.index < len(self.data):
            byte = self.data[self.index]
            self.index += 1
            value |= (byte & 0x7F) << shift
            if byte < 0x80:
                return value
            shift += 7
            if shift >= 70:
                raise ProtoError("varint is too long")
        raise ProtoError("truncated varint")

    def read_bytes(self, size: int) -> bytes:
        if size < 0 or self.index + size > len(self.data):
            raise ProtoError("truncated field")
        value = self.data[self.index : self.index + size]
        self.index += size
        return value

    def skip_group(self) -> None:
        while not self.eof():
            key = self.read_varint()
            wire_type = key & 7
            if wire_type == 4:
                return
            self.skip_value(wire_type)
        raise ProtoError("unterminated group")

    def skip_value(self, wire_type: int) -> None:
        match wire_type:
            case 0:
                self.read_varint()
            case 1:
                self.read_bytes(8)
            case 2:
                self.read_bytes(self.read_varint())
            case 3:
                self.skip_group()
            case 4:
                return
            case 5:
                self.read_bytes(4)
            case _:
                raise ProtoError(f"unsupported protobuf wire type {wire_type}")

    def fields(self) -> Iterable[Field]:
        while not self.eof():
            key = self.read_varint()
            if key == 0:
                raise ProtoError("invalid field key 0")
            number = key >> 3
            wire_type = key & 7
            match wire_type:
                case 0:
                    yield Field(number, wire_type, self.read_varint())
                case 1:
                    yield Field(number, wire_type, self.read_bytes(8))
                case 2:
                    yield Field(number, wire_type, self.read_bytes(self.read_varint()))
                case 3:
                    self.skip_group()
                case 4:
                    raise ProtoError("unexpected end-group field")
                case 5:
                    yield Field(number, wire_type, self.read_bytes(4))
                case _:
                    raise ProtoError(f"unsupported protobuf wire type {wire_type}")


def parse_fields(data: bytes) -> Iterable[Field]:
    return ProtoReader(data).fields()


def parse_uint256(data: bytes) -> UInt256:
    value = UInt256()
    for field in parse_fields(data):
        if field.wire_type != 0 or not isinstance(field.value, int):
            continue
        match field.number:
            case 1:
                value.v1 = field.value & ((1 << 64) - 1)
            case 2:
                value.v2 = field.value & ((1 << 64) - 1)
            case 3:
                value.v3 = field.value & ((1 << 64) - 1)
            case 4:
                value.v4 = field.value & ((1 << 64) - 1)
    return value


def parse_storage(data: bytes) -> StorageEntry:
    storage = StorageEntry()
    for field in parse_fields(data):
        if field.wire_type != 2 or not isinstance(field.value, bytes):
            continue
        match field.number:
            case 1:
                storage.key = parse_uint256(field.value)
            case 2:
                storage.value = parse_uint256(field.value)
    return storage


def parse_account(data: bytes) -> Account:
    account = Account()
    for field in parse_fields(data):
        match field.number, field.wire_type, field.value:
            case 1, 2, bytes(value):
                account.address = parse_uint256(value)
            case 2, 2, bytes(value):
                account.code = value
            case 3, 0, int(value):
                account.balance = value
            case 4, 0, int(value):
                account.nonce = value
            case 5, 2, bytes(value):
                account.storage.append(parse_storage(value))
    return account


def parse_block(data: bytes) -> Block:
    block = Block()
    for field in parse_fields(data):
        match field.number, field.wire_type, field.value:
            case 1, 0, int(value):
                block.number = value
            case 2, 0, int(value):
                block.timestamp = value
            case 3, 0, int(value):
                block.gaslimit = value
            case 4, 2, bytes(value):
                block.coinbase = parse_uint256(value)
            case 5, 2, bytes(value):
                block.difficulty = parse_uint256(value)
            case 7, 0, int(value):
                block.basefee = value
            case 8, 0, int(value):
                block.excessblobgas = value
    return block


def parse_access_list_entry(data: bytes) -> AccessListEntry:
    entry = AccessListEntry()
    for field in parse_fields(data):
        if field.wire_type != 2 or not isinstance(field.value, bytes):
            continue
        match field.number:
            case 1:
                entry.address = parse_uint256(field.value)
            case 2:
                entry.keys.append(parse_uint256(field.value))
    return entry


def parse_transaction(data: bytes) -> Transaction:
    tx = Transaction()
    for field in parse_fields(data):
        match field.number, field.wire_type, field.value:
            case 1, 2, bytes(value):
                tx.sender = parse_uint256(value)
            case 2, 2, bytes(value):
                tx.recipient = parse_uint256(value)
            case 3, 2, bytes(value):
                tx.data = value
            case 4, 0, int(value):
                tx.gas = value
            case 5, 0, int(value):
                tx.value = value
            case 6, 0, int(value):
                tx.creates = value != 0
            case 7, 2, bytes(value):
                tx.access_list.append(parse_access_list_entry(value))
    return tx


def parse_prestate(data: bytes) -> Prestate:
    prestate = Prestate()
    for field in parse_fields(data):
        if field.wire_type != 2 or not isinstance(field.value, bytes):
            continue
        match field.number:
            case 1:
                prestate.accounts.append(parse_account(field.value))
            case 2:
                prestate.block = parse_block(field.value)
            case 3:
                prestate.txs.append(parse_transaction(field.value))
    return prestate


def source_items(source: Path) -> Iterable[tuple[str, bytes]]:
    if source.is_dir():
        for path in sorted(path for path in source.rglob("*") if path.is_file()):
            yield path.relative_to(source).as_posix(), path.read_bytes()
        return

    with tarfile.open(source) as archive:
        for member in archive:
            if not member.isfile():
                continue
            extracted = archive.extractfile(member)
            if extracted is None:
                continue
            yield Path(member.name).name, extracted.read()


def evm_spec(block_number: int, timestamp: int) -> str:
    if block_number < 1_150_000:
        return "FRONTIER"
    if block_number < 2_463_000:
        return "HOMESTEAD"
    if block_number < 2_675_000:
        return "TANGERINE"
    if block_number < 4_370_000:
        return "SPURIOUS_DRAGON"
    if block_number < 7_280_000:
        return "BYZANTIUM"
    if block_number < 9_069_000:
        return "PETERSBURG"
    if block_number < 12_244_000:
        return "ISTANBUL"
    if block_number < 12_965_000:
        return "BERLIN"
    if block_number < 15_537_393:
        return "LONDON"
    if timestamp < 1_681_338_455:
        return "MERGE"
    if timestamp < 1_710_338_135:
        return "SHANGHAI"
    if timestamp < 1_800_000_000:
        return "CANCUN"
    return "PRAGUE"


def spec_supports_access_list(spec: str) -> bool:
    return spec in {"BERLIN", "LONDON", "MERGE", "SHANGHAI", "CANCUN", "PRAGUE", "OSAKA", "AMSTERDAM"}


def spec_supports_istanbul_calldata(spec: str) -> bool:
    return spec in {"ISTANBUL", "BERLIN", "LONDON", "MERGE", "SHANGHAI", "CANCUN", "PRAGUE", "OSAKA", "AMSTERDAM"}


def spec_supports_create_intrinsic(spec: str) -> bool:
    return spec != "FRONTIER"


def spec_supports_shanghai(spec: str) -> bool:
    return spec in {"SHANGHAI", "CANCUN", "PRAGUE", "OSAKA", "AMSTERDAM"}


def spec_supports_prague(spec: str) -> bool:
    return spec in {"PRAGUE", "OSAKA", "AMSTERDAM"}


def access_list_counts(tx: Transaction) -> tuple[int, int]:
    accounts = len(tx.access_list)
    keys = sum(len(entry.keys) for entry in tx.access_list)
    return accounts, keys


def intrinsic_gas(spec: str, tx: Transaction) -> int:
    nonzero_cost = 16 if spec_supports_istanbul_calldata(spec) else 68
    gas = 21_000
    for byte in tx.data:
        gas += 4 if byte == 0 else nonzero_cost
    if spec_supports_access_list(spec):
        accounts, keys = access_list_counts(tx)
        gas += accounts * 2_400 + keys * 1_900
    if tx.creates and spec_supports_create_intrinsic(spec):
        gas += 32_000
    if tx.creates and spec_supports_shanghai(spec):
        gas += 2 * ((len(tx.data) + 31) // 32)
    return gas


def floor_gas(spec: str, tx: Transaction) -> int:
    if not spec_supports_prague(spec):
        return 0
    tokens = 0
    for byte in tx.data:
        tokens += 1 if byte == 0 else 4
    return 21_000 + tokens * 10


def tx_gas_limit(spec: str, tx: Transaction) -> int:
    initial = max(intrinsic_gas(spec, tx), floor_gas(spec, tx))
    gas_limit = tx.gas + initial
    if spec == "OSAKA":
        gas_limit = min(gas_limit, OSAKA_TX_GAS_LIMIT_CAP)
    return gas_limit


def validate_prestate(prestate: Prestate) -> str | None:
    if not prestate.txs:
        return "zero transactions"
    if len(prestate.txs) > MAX_TXS:
        return "too many transactions"
    if len(prestate.accounts) > MAX_ACCOUNTS:
        return "too many accounts"
    if prestate.block.excessblobgas > MAX_EXCESS_BLOB_GAS:
        return "excess blob gas too large"
    if prestate.block.number == (1 << 64) - 1:
        return "block number is u64::MAX"
    if prestate.block.coinbase.is_special_address():
        return "coinbase has reserved address"

    addresses: set[str] = set()
    for account in prestate.accounts:
        address = account.address.address()
        if address in addresses:
            return "duplicate account"
        addresses.add(address)
        if len(account.storage) > MAX_STORAGE_PER_ACCOUNT:
            return "too many storage entries"
        if len(account.code) > MAX_DATA_SIZE:
            return "account code too large"
        if account.nonce > MAX_NONCE:
            return "account nonce too large"
        if account.balance == 0:
            return "account has no balance"
        if account.address.is_special_address():
            return "account has reserved address"

    total_gas = 0
    for tx in prestate.txs:
        if len(tx.access_list) > MAX_ACCESS_LIST_PER_TX:
            return "too many access list entries"
        if len(tx.data) > MAX_DATA_SIZE:
            return "transaction data too large"
        if any(len(entry.keys) > MAX_ACCESS_LIST_KEYS_PER_TX for entry in tx.access_list):
            return "too many access list keys"
        if tx.gas > MAX_GAS:
            return "transaction gas too large"
        if tx.sender.is_special_address():
            return "sender has reserved address"
        total_gas += tx.gas
        if total_gas > MAX_GAS:
            return "aggregate transaction gas too large"
    return None


def access_list_json(tx: Transaction, keep: bool) -> list[dict[str, Any]]:
    if not keep:
        return []
    return [
        {
            "address": entry.address.address(),
            "storageKeys": [key.b256_hex() for key in entry.keys],
        }
        for entry in tx.access_list
    ]


def tx_json(spec: str, tx: Transaction, nonce: int) -> dict[str, Any]:
    keep_access_list = spec_supports_access_list(spec)
    return {
        "kind": "Eip2930" if keep_access_list else "Legacy",
        "caller": tx.sender.address(),
        "target": tx.recipient.address(),
        "creates": tx.creates,
        "gas_limit": tx_gas_limit(spec, tx),
        "gas_price": 0,
        "value": quantity(tx.value),
        "input": bytes_hex(tx.data),
        "nonce": nonce,
        "access_list": access_list_json(tx, keep_access_list),
        "blob_hashes": [],
    }


def account_json(account: Account) -> dict[str, Any]:
    storage: dict[str, str] = {}
    for entry in account.storage:
        storage[entry.key.word_hex()] = entry.value.word_hex()
    return {
        "address": account.address.address(),
        "balance": quantity(account.balance),
        "nonce": account.nonce,
        "code": bytes_hex(account.code),
        "storage": storage,
    }


def convert_prestate(prestate: Prestate, *, validate: bool) -> tuple[dict[str, Any] | None, str | None]:
    if validate and (reason := validate_prestate(prestate)):
        return None, reason

    spec = evm_spec(prestate.block.number + 1, prestate.block.timestamp)
    account_by_address = {account.address.address(): account for account in prestate.accounts}
    next_nonce = {address: account.nonce for address, account in account_by_address.items()}
    txs = []
    for tx in prestate.txs:
        sender = tx.sender.address()
        account = account_by_address.get(sender)
        if account is None or account.balance == 0:
            continue
        nonce = next_nonce.get(sender, 0)
        txs.append(tx_json(spec, tx, nonce))
        next_nonce[sender] = min(nonce + 1, (1 << 64) - 1)

    if not txs:
        return None, "no executable transactions"

    max_tx_gas = max(tx["gas_limit"] for tx in txs)
    block_gas_limit = max(prestate.block.gaslimit, max_tx_gas)
    features = ["imported_evm_protobuf"]
    if prestate.block.coinbase.address() != BENEFICIARY:
        features.append("protobuf_coinbase_dropped")
    if prestate.block.difficulty.word() != 0:
        features.append("protobuf_difficulty_dropped")
    if prestate.block.excessblobgas != 0:
        features.append("protobuf_excess_blob_gas_dropped")
    if prestate.block.basefee != 0:
        features.append("protobuf_basefee_zeroed")
    if block_gas_limit != prestate.block.gaslimit:
        features.append("protobuf_block_gas_limit_raised")

    return {
        "spec": spec,
        "block": {
            "number": quantity(prestate.block.number + 1),
            "timestamp": quantity(prestate.block.timestamp),
            "gas_limit": block_gas_limit,
            "basefee": 0,
        },
        "tx": txs[0],
        "extra_txs": txs[1:],
        "features": features,
        "accounts": [account_json(account) for account in prestate.accounts],
    }, None


def output_path(output: Path, name: str) -> Path:
    stem = Path(name).name or "case"
    return output / f"protobuf-{stem}.json"


def write_case(path: Path, case: dict[str, Any], *, force: bool) -> bool:
    existed = path.exists()
    if existed and not force:
        return True
    path.write_text(json.dumps(case, indent=2) + "\n")
    return existed


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", nargs="?", type=Path, default=DEFAULT_SOURCE, help="Corpus tar.xz or extracted directory")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT, help="Directory for converted JSON cases")
    parser.add_argument("--limit", type=int, help="Convert at most this many source entries")
    parser.add_argument("--force", action="store_true", help="Overwrite existing converted cases")
    parser.add_argument("--clean", action="store_true", help="Remove the output directory before conversion")
    parser.add_argument(
        "--no-validate",
        action="store_true",
        help="Do not apply evm-protobuf-fuzzer validation filters before conversion",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    source = args.source.expanduser().resolve()
    output = args.output.expanduser().resolve()

    if not source.exists():
        raise SystemExit(f"source does not exist: {source}")
    if args.clean and output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True, exist_ok=True)

    stats = Stats()
    for name, data in source_items(source):
        if args.limit is not None and stats.seen >= args.limit:
            break
        stats.seen += 1
        try:
            prestate = parse_prestate(data)
        except ProtoError:
            stats.skipped_parse += 1
            continue
        case, reason = convert_prestate(prestate, validate=not args.no_validate)
        if case is None:
            if reason == "no executable transactions":
                stats.skipped_no_txs += 1
            elif reason == "duplicate account":
                stats.skipped_duplicate += 1
            else:
                stats.skipped_validate += 1
            continue
        existed = write_case(output_path(output, name), case, force=args.force)
        if existed:
            stats.overwritten += int(args.force)
        stats.converted += 1

    print(f"source: {source}")
    print(f"output: {output}")
    print(f"seen: {stats.seen}")
    print(f"converted: {stats.converted}")
    print(f"skipped_parse: {stats.skipped_parse}")
    print(f"skipped_validate: {stats.skipped_validate}")
    print(f"skipped_duplicate: {stats.skipped_duplicate}")
    print(f"skipped_no_txs: {stats.skipped_no_txs}")
    if args.force:
        print(f"overwritten: {stats.overwritten}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
