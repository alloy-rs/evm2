#!/usr/bin/env python3

import json


class Case:
    name: str
    kind: str
    os: str
    rust: str
    flags: str
    fixture: str
    target: str
    command: str
    rustflags: str

    def __init__(
        self,
        name: str,
        kind: str,
        os: str,
        rust: str,
        flags: str = "",
        fixture: str = "",
        target: str = "",
        command: str = "nextest",
        rustflags: str = "",
    ):
        self.name = name
        self.kind = kind
        self.os = os
        self.rust = rust
        self.flags = flags
        self.fixture = fixture
        self.target = target
        self.command = command
        self.rustflags = rustflags


def default_cases():
    cases = []
    for kind in ["test", "eest"]:
        for rust in ["stable", "nightly"]:
            for flags in ["--no-default-features", ""]:
                name = f"{kind} +{rust} ubuntu-latest"
                if flags:
                    name += f" {flags}"
                cases.append(Case(name=name, kind=kind, os="ubuntu-latest", rust=rust, flags=flags))

    return cases


def extra_cases():
    return [
        Case(
            name="test +nightly ubuntu-latest --all-features",
            kind="test",
            os="ubuntu-latest",
            rust="nightly",
            flags="--all-features",
        ),
        Case(
            name="eest +nightly ubuntu-latest --all-features",
            kind="eest",
            os="ubuntu-latest",
            rust="nightly",
            flags="--all-features",
        ),
        Case(
            name="wasm +stable ubuntu-latest wasm32-unknown-unknown --no-default-features",
            kind="wasm",
            os="ubuntu-latest",
            rust="stable",
            flags="--no-default-features",
            target="wasm32-unknown-unknown",
            command="build",
        ),
        Case(
            name="wasm wasi +stable ubuntu-latest wasm32-wasip1 --no-default-features --features no-tco",
            kind="wasm",
            os="ubuntu-latest",
            rust="stable",
            flags="--no-default-features --features no-tco",
            fixture="wasi",
            target="wasm32-wasip1",
            command="wasm-test",
        ),
        Case(
            name="wasm wasi-tail +stable ubuntu-latest wasm32-wasip1 --no-default-features",
            kind="wasm",
            os="ubuntu-latest",
            rust="stable",
            flags="--no-default-features",
            fixture="wasi-tail",
            target="wasm32-wasip1",
            command="wasm-test",
            rustflags="-Ctarget-feature=+simd128,+tail-call",
        ),
        Case(
            name="test i686 +stable ubuntu-latest i686-unknown-linux-gnu",
            kind="test",
            os="ubuntu-latest",
            rust="stable",
            fixture="i686",
            target="i686-unknown-linux-gnu",
            command="cross-test",
        ),
        Case(
            name="test armv7 +stable ubuntu-latest armv7-unknown-linux-gnueabihf",
            kind="test",
            os="ubuntu-latest",
            rust="stable",
            fixture="armv7",
            target="armv7-unknown-linux-gnueabihf",
            command="cross-test",
        ),
        Case(
            name="test +stable ubuntu-24.04-arm",
            kind="test",
            os="ubuntu-24.04-arm",
            rust="stable",
        ),
    ]


def main():
    cases = default_cases() + extra_cases()
    print_json({"include": [vars(case) for case in cases]})


def print_json(obj):
    print(json.dumps(obj), end="", flush=True)


if __name__ == "__main__":
    main()
