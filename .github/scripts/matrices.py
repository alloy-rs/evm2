#!/usr/bin/env python3

import json


# A runner target.
class Target:
    # GHA runner.
    runner_label: str
    # Rust target triple.
    target: str

    def __init__(self, runner_label: str, target: str):
        self.runner_label = runner_label
        self.target = target


# A single CI suite to run.
class Case:
    # Name of the suite.
    name: str
    # Test kind.
    kind: str
    # Rust toolchain.
    rust: str
    # Cargo flags.
    flags: str
    # Fixture name.
    fixture: str
    # Command selector.
    command: str
    # Extra RUSTFLAGS.
    rustflags: str

    def __init__(
        self,
        name: str,
        kind: str,
        rust: str,
        flags: str,
        command: str = "nextest",
        fixture: str = "",
        rustflags: str = "",
    ):
        self.name = name
        self.kind = kind
        self.rust = rust
        self.flags = flags
        self.fixture = fixture
        self.command = command
        self.rustflags = rustflags


# GHA matrix entry.
class Expanded:
    name: str
    runner_label: str
    kind: str
    rust: str
    flags: str
    fixture: str
    target: str
    command: str
    rustflags: str

    def __init__(
        self,
        name: str,
        runner_label: str,
        kind: str,
        rust: str,
        flags: str,
        fixture: str,
        target: str,
        command: str,
        rustflags: str,
    ):
        self.name = name
        self.runner_label = runner_label
        self.kind = kind
        self.rust = rust
        self.flags = flags
        self.fixture = fixture
        self.target = target
        self.command = command
        self.rustflags = rustflags


t_linux_x86 = Target("ubuntu-latest", "")
t_linux_arm = Target("ubuntu-24.04-arm", "")
t_wasm_unknown = Target("ubuntu-latest", "wasm32-unknown-unknown")
t_wasm_wasi = Target("ubuntu-latest", "wasm32-wasip1")
t_linux_i686 = Target("ubuntu-latest", "i686-unknown-linux-gnu")
t_linux_armv7 = Target("ubuntu-latest", "armv7-unknown-linux-gnueabihf")

config = [
    (
        t_linux_x86,
        Case(
            "test +stable ubuntu-latest --no-default-features",
            "test",
            "stable",
            "--no-default-features",
        ),
    ),
    (t_linux_x86, Case("test +stable ubuntu-latest", "test", "stable", "")),
    (
        t_linux_x86,
        Case(
            "test +nightly ubuntu-latest --no-default-features",
            "test",
            "nightly",
            "--no-default-features",
        ),
    ),
    (t_linux_x86, Case("test +nightly ubuntu-latest", "test", "nightly", "")),
    (
        t_linux_x86,
        Case(
            "eest +stable ubuntu-latest --no-default-features",
            "eest",
            "stable",
            "--no-default-features",
        ),
    ),
    (t_linux_x86, Case("eest +stable ubuntu-latest", "eest", "stable", "")),
    (
        t_linux_x86,
        Case(
            "eest +nightly ubuntu-latest --no-default-features",
            "eest",
            "nightly",
            "--no-default-features",
        ),
    ),
    (t_linux_x86, Case("eest +nightly ubuntu-latest", "eest", "nightly", "")),
    (
        t_linux_x86,
        Case(
            "test +nightly ubuntu-latest --all-features",
            "test",
            "nightly",
            "--all-features",
        ),
    ),
    (
        t_linux_x86,
        Case(
            "eest +nightly ubuntu-latest --all-features",
            "eest",
            "nightly",
            "--all-features",
        ),
    ),
    (
        t_wasm_unknown,
        Case(
            "wasm +stable ubuntu-latest wasm32-unknown-unknown --no-default-features",
            "wasm",
            "stable",
            "--no-default-features",
            command="build",
        ),
    ),
    (
        t_wasm_wasi,
        Case(
            "wasm wasi +stable ubuntu-latest wasm32-wasip1 --no-default-features --features no-tco",
            "wasm",
            "stable",
            "--no-default-features --features no-tco",
            command="wasm-test",
            fixture="wasi",
        ),
    ),
    (
        t_wasm_wasi,
        Case(
            "wasm wasi-tail +stable ubuntu-latest wasm32-wasip1 --no-default-features",
            "wasm",
            "stable",
            "--no-default-features",
            command="wasm-test",
            fixture="wasi-tail",
            rustflags="-Ctarget-feature=+simd128,+tail-call",
        ),
    ),
    (
        t_linux_i686,
        Case(
            "test i686 +stable ubuntu-latest i686-unknown-linux-gnu",
            "test",
            "stable",
            "",
            command="cross-test",
            fixture="i686",
        ),
    ),
    (
        t_linux_armv7,
        Case(
            "test armv7 +stable ubuntu-latest armv7-unknown-linux-gnueabihf",
            "test",
            "stable",
            "",
            command="cross-test",
            fixture="armv7",
        ),
    ),
    (t_linux_arm, Case("test +stable ubuntu-24.04-arm", "test", "stable", "")),
]


def main():
    expanded = []
    for target, case in config:
        obj = Expanded(
            name=case.name,
            runner_label=target.runner_label,
            kind=case.kind,
            rust=case.rust,
            flags=case.flags,
            fixture=case.fixture,
            target=target.target,
            command=case.command,
            rustflags=case.rustflags,
        )
        expanded.append(vars(obj))

    print_json({"include": expanded})


def print_json(obj):
    print(json.dumps(obj), end="", flush=True)


if __name__ == "__main__":
    main()
