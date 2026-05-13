#!/usr/bin/env python3

import json


# A runner target.
class Target:
    # Human-readable target name.
    name: str
    # GHA runner.
    runner_label: str
    # Rust target triple.
    target: str
    # Matrix tier.
    tier: int
    # Command selector.
    command: str
    # Test kinds to run.
    kinds: list[str]
    # Tier 2 Cargo feature flags.
    flags: str
    # Fixture name.
    fixture: str
    # Extra RUSTFLAGS.
    rustflags: str
    # Extra CXX.
    cxx: str

    def __init__(
        self,
        name: str,
        runner_label: str,
        target: str = "",
        tier: int = 2,
        command: str = "nextest",
        kinds: list[str] | None = None,
        flags: str = "",
        fixture: str = "",
        rustflags: str = "",
        cxx: str = "",
    ):
        self.name = name
        self.runner_label = runner_label
        self.target = target
        self.tier = tier
        self.command = command
        self.kinds = ["test", "eest"] if kinds is None else kinds
        self.flags = flags
        self.fixture = fixture
        self.rustflags = rustflags
        self.cxx = cxx


# A single CI suite to run.
class Case:
    # Test kind.
    kind: str
    # Rust toolchain.
    rust: str
    # Cargo flags.
    flags: str

    def __init__(self, kind: str, rust: str, flags: str):
        self.kind = kind
        self.rust = rust
        self.flags = flags


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
    cxx: str

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
        cxx: str,
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
        self.cxx = cxx


toolchains = ["stable", "nightly"]
feature_sets = ["--no-default-features", "", "--all-features"]
kinds = ["test", "eest"]

t_linux_x86 = Target("ubuntu", "ubuntu-latest", tier=1)
t_macos_arm = Target("macos", "macos-latest", tier=1)
t_linux_arm = Target("ubuntu arm", "ubuntu-24.04-arm", cxx="clang++")
t_windows = Target("windows", "windows-latest", flags="--no-default-features")
t_wasm_unknown = Target(
    "wasm",
    "ubuntu-latest",
    target="wasm32-unknown-unknown",
    command="build",
    kinds=["wasm"],
    flags="--no-default-features",
)
t_wasm_wasi = Target(
    "wasm wasi",
    "ubuntu-latest",
    target="wasm32-wasip1",
    command="wasm-test",
    kinds=["wasm"],
    flags="--no-default-features --features no-tco",
    fixture="wasi",
)
t_wasm_wasi_tail = Target(
    "wasm tail-call",
    "ubuntu-latest",
    target="wasm32-wasip1",
    command="wasm-test",
    kinds=["wasm"],
    flags="--no-default-features",
    fixture="wasi-tail",
    rustflags="-Ctarget-feature=+simd128,+tail-call",
)
t_linux_i686 = Target(
    "i686",
    "ubuntu-latest",
    target="i686-unknown-linux-gnu",
    command="cross-test",
    kinds=["test"],
    flags="--no-default-features",
)
t_linux_armv7 = Target(
    "armv7",
    "ubuntu-latest",
    target="armv7-unknown-linux-gnueabihf",
    command="cross-test",
    kinds=["test"],
    flags="--no-default-features",
)

targets = [
    t_linux_x86,
    # t_macos_arm,
    # t_linux_arm,
    t_windows,
    # t_wasm_unknown,
    # t_wasm_wasi,
    # t_wasm_wasi_tail,
    # t_linux_i686,
    # t_linux_armv7,
]

config = [
    Case(kind=kind, rust=rust, flags=flags)
    for kind in kinds
    for rust in toolchains
    for flags in feature_sets
]


def main():
    expanded = []
    for target in targets:
        for case in target_cases(target):
            flags = target.flags if target.tier == 2 else case.flags
            obj = Expanded(
                name=name(target, case, flags),
                runner_label=target.runner_label,
                kind=case.kind,
                rust=case.rust,
                flags=flags,
                fixture=target.fixture,
                target=target.target,
                command=target.command,
                rustflags=target.rustflags,
                cxx=target.cxx,
            )
            expanded.append(vars(obj))

    expanded.sort(key=lambda obj: obj["name"])
    print_json({"include": expanded})


def target_cases(target: Target):
    if target.tier == 1:
        return [case for case in config if case.kind in target.kinds]

    return [Case(kind=kind, rust="stable", flags=target.flags) for kind in target.kinds]


def name(target: Target, case: Case, flags: str):
    s = f"{case.kind} +{case.rust} {target.name}"
    if flags:
        s += f" {flags}"
    return s


def print_json(obj):
    print(json.dumps(obj), end="", flush=True)


if __name__ == "__main__":
    main()
