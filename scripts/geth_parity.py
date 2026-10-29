#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Checks that evm2 and go-ethereum agree exactly on EVM results and tracer output.

Each case (inputs of `evm t8n`: alloc.json, env.json, txs.json) runs through go-ethereum's `evm t8n`
and through `evm2 t8n` for every fork and tracer of the matrix. The JSON of both sides is compared
field by field: the block result (state root, receipts, logs, rejected transactions), the post
state, and for each executed transaction the tracer output. Differences print as one line each
with case, fork, tracer, transaction, path, and the geth and evm2 values.

Quick start (from the repository root):

    scripts/geth_parity.py                              # every committed case, every fork and tracer
    scripts/geth_parity.py call_nested                  # one case (substring or glob of the name)
    scripts/geth_parity.py oog_* --fork Cancun --tracer jsonl --tracer structlog
    scripts/geth_parity.py --offline                    # no geth: compare against stored outputs
    scripts/geth_parity.py --list                       # list cases, forks and tracers

Tracer matrix (`--tracer ID`, repeatable; the list is crates/cli/testdata/geth-parity/tracers.json):
result (block result, receipts, requests, block access list and post state), jsonl (EIP-3155 lines
with memory and return data), call, call-log, call-top, prestate, prestate-diff, 4byte, flat, mux,
noop, js-opcount, js-frames, js-steps (JavaScript tracers of the test data), structlog (the default
struct logger frame, as `debug_traceTransaction` returns it). evm2 runs every tracer through
`evm2 t8n`. Geth runs result and jsonl through `evm t8n`; it runs all other tracers through
`debug_traceTransaction` on a local `geth --dev` node that has the case fork and alloc as its
genesis and executes the transactions that t8n accepted, because t8n shares one tracer between
the transactions and the system call of the block, which breaks the call tracers from Cancun on.
The node needs Paris or later and no env opcodes; a case sets `"node": false` to leave the node
tracers out. `--js-dir DIR` adds each `*.js` file of DIR (for example
`eth/tracers/js/internal/tracers` of a go-ethereum checkout) as js:<name>.

Known differences: `--known-diffs FILE` (default crates/cli/testdata/geth-parity/known-diffs.json)
lists differences that exist today as {case, fork, tracer, path, op?, reason}, plus `"kind": "geth-gap"` for a
difference where geth lacks a feature that evm2 has and `"kind": "extension"` for a field evm2 prints
and geth does not (the summary counts both apart, `--write-known-diffs` keeps them as they are, and
fix PRs never delete them). Every field but reason
may be a list. Case, fork and tracer are globs (`*` matches any text) and path is a glob over the
field path, with array indexes written as `[]`, addresses as `<address>` and hashes as `<hash>`,
for example `structLogs[].gasCost`. An optional `op` (a list of opcode names, globs) limits an entry
to differences in steps of those opcodes. The exit status is non-zero for a difference that is not
listed, for a listed difference that no longer occurs (delete the entry: a fix removes entries,
which proves it), and for an entry whose reason is empty or `unclassified`. `--write-known-diffs` rewrites the file from the current differences of
the selected run, keeping the reasons of entries with the same tracer and path.

Outputs: geth is go-ethereum master at a pinned commit (it has the Amsterdam fork), fetched and
built once into ~/.cache/geth-parity/geth-<commit> (override with GETH_PARITY_CACHE; a pinned Go
toolchain is downloaded and verified when `go` is missing or older than go.mod needs).
`--geth-dir DIR` reuses prebuilt `evm` and `geth` binaries instead. evm2 is built with
`cargo build -p evm2-cli` unless `--evm2 PATH` names a binary. `--keep DIR` keeps all raw outputs
of both tools. `--bless` stores geth's outputs as case/expected/<Fork>.json so that the Rust test
`cargo nextest run -p evm2-cli --test geth_parity` and `--offline` work without geth.

EEST mode: `--eest PATH` converts state test fixtures (a file, or a directory searched for *.json,
for example test-fixtures/devnet/state_tests; see scripts/setup_test_fixtures.py, or symlink the
test-fixtures directory of another worktree) into one t8n input per test, fork and index, and
checks them. It compares the block result and post state, and the tracers named with `--tracer`.
Names are <file>::<test>::<fork>::<indexes>, filtered by the same positional patterns. EEST
mode runs result unless `--tracer jsonl` is given (the node tracers need a case that avoids env
opcodes). A test that both tools refuse to decode counts as skipped.

Normalisation before comparing: hex strings compare in lower case, and the rejected transactions
compare by index (`--strict-errors` also compares the messages). Everything else compares exactly.
"""

import argparse
import concurrent.futures as futures
import fnmatch
import functools
import itertools
import hashlib
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "crates/cli/testdata/geth-parity"

# go-ethereum master, built from source: it has the Amsterdam fork (glamsterdam-devnet-8).
GETH_REPO = "https://github.com/ethereum/go-ethereum"
GETH_SHA = "a0cba163b1f17f6044df84502ff13036dffb5de1"
GETH_VERSION = GETH_SHA[:8]
GO_VERSION = "1.27.3"
GO_SHA256 = {
    ("linux", "amd64"): "65efbc80fdc1fa2cd2631cad5b481777a68e8725f34e89f50fc6ac714e379b40",
    ("linux", "arm64"): "a06e99309dbcbdca1a8ccd2282c3ee146e55930886e26b4f1193269587165f49",
    ("darwin", "amd64"): "b6f8e931e4ab497fcde6da8ae4499a58f9d258e55c8a80824a516eae5950a750",
    ("darwin", "arm64"): "dd3908cf20959d798c1e06ebe5b72213f0505ce01373b40ce3110975a6cd99c2",
}

DEFAULT_FORKS = ["Cancun", "Prague", "Osaka", "Amsterdam"]
NODE_MIN_FORK = "Merge"

# Tracer id -> {kind, tracer, js, config}, shared with the Rust test. `result` compares the block
# result and post state only; `jsonl` is the EIP-3155 stream; `structlog` the default struct
# logger frame; `tracer` entries are geth tracers by name or JavaScript file.
TRACER_SPECS = json.loads((DATA / "tracers.json").read_text())
DEFAULT_TRACERS = list(TRACER_SPECS)
STRUCTLOG = "structlog"

# Everything but result and jsonl runs through `debug_traceTransaction` on a geth node for geth:
# the t8n of geth shares one tracer between the transactions and the system call of the block,
# which corrupts the call tracer family.
T8N_TRACERS = ("result", "jsonl")

# Prefix of the error of a run that both tools reject, such as an EEST test of an invalid transaction.
REJECTED = "both tools reject the input"
CRASHED = "crashed: "  # starts the error of a tool that timed out, died on a signal or panicked


def tracer_source(spec):
    path = Path(spec["js"])
    return (path if path.is_absolute() else DATA / path).read_text()


def tracer_name(spec):
    """The geth tracer name, or the source of a JavaScript tracer."""
    return tracer_source(spec) if "js" in spec else spec["tracer"]


def evm2_flags(tracer):
    spec = TRACER_SPECS[tracer]
    if spec["kind"] == "jsonl":
        return ["--trace", "--trace.memory", "--trace.returndata"]
    if spec["kind"] == "structlog":
        return ["--trace.structlog", "--trace.memory", "--trace.returndata"]
    if spec["kind"] == "tracer":
        return ["--trace.tracer", tracer_name(spec), "--trace.jsonconfig", json.dumps(spec["config"])]
    return []


def node_options(tracer):
    spec = TRACER_SPECS[tracer]
    if spec["kind"] == "structlog":
        return {"enableMemory": True, "enableReturnData": True}
    return {"tracer": tracer_name(spec), "tracerConfig": spec["config"]}


def log(message):
    print(message, file=sys.stderr, flush=True)


# --- geth and evm2 binaries


def cache_dir():
    return Path(os.environ.get("GETH_PARITY_CACHE", Path.home() / ".cache/geth-parity"))


def go_binary(min_version):
    """Returns a `go` that is at least `min_version`: the one on PATH, or the pinned toolchain."""
    found = shutil.which("go")
    if found:
        out = run([found, "version"]).stdout
        match = re.search(r"go(\d+)\.(\d+)", out)
        if match and (int(match[1]), int(match[2])) >= min_version:
            return found
    system = (sys.platform, platform.machine().lower().replace("x86_64", "amd64").replace("aarch64", "arm64"))
    if system not in GO_SHA256:
        sys.exit(f"no pinned Go toolchain for {system}; install Go >= {min_version[0]}.{min_version[1]}")
    target = cache_dir() / f"go{GO_VERSION}"
    if not (target / "bin/go").exists():
        archive = cache_dir() / f"go{GO_VERSION}.tar.gz"
        url = f"https://go.dev/dl/go{GO_VERSION}.{system[0]}-{system[1]}.tar.gz"
        log(f"downloading {url}")
        cache_dir().mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(url, timeout=120) as response, open(archive, "wb") as out:
            shutil.copyfileobj(response, out)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != GO_SHA256[system]:
            archive.unlink()
            sys.exit(f"sha256 mismatch for {url}: {digest}")
        staging = Path(tempfile.mkdtemp(dir=cache_dir()))
        with tarfile.open(archive) as tar:
            tar.extractall(staging, filter="data")
        (staging / "go").rename(target)
        staging.rmdir()
        archive.unlink()
    return str(target / "bin/go")


def ensure_geth(geth_dir=None):
    """Returns the directory with go-ethereum's `evm` and `geth`.

    `--geth-dir` names prebuilt binaries. Otherwise go-ethereum is fetched at the pinned commit
    and built once per commit, with a pinned Go toolchain when `go` is missing or too old.
    """
    if geth_dir:
        path = Path(geth_dir).expanduser().resolve()
        for name in ("evm", "geth"):
            if not (path / name).exists():
                sys.exit(f"{path / name} not found")
        return path
    target = cache_dir() / f"geth-{GETH_VERSION}"
    if (target / "evm").exists() and (target / "geth").exists():
        return target
    source = cache_dir() / "go-ethereum"
    source.mkdir(parents=True, exist_ok=True)
    git = lambda *a: subprocess.run(["git", "-C", str(source), *a], check=True, capture_output=True, text=True)
    if not (source / ".git").exists():
        git("init", "-q")
    log(f"fetching go-ethereum {GETH_SHA}")
    git("fetch", "-q", "--depth", "1", GETH_REPO, GETH_SHA)
    git("checkout", "-q", "--detach", GETH_SHA)
    match = re.search(r"^go (\d+)\.(\d+)", (source / "go.mod").read_text(), re.M)
    go = go_binary((int(match[1]), int(match[2])))
    staging = Path(tempfile.mkdtemp(dir=cache_dir()))
    log(f"building evm and geth with {go}")
    for name in ("evm", "geth"):
        build = subprocess.run([go, "build", "-o", str(staging / name), f"./cmd/{name}"], cwd=source, capture_output=True, text=True)
        if build.returncode != 0:
            sys.exit(f"go build ./cmd/{name} failed:\n{build.stderr[-2000:]}")
    if target.exists():
        shutil.rmtree(target)
    staging.rename(target)
    return target


def ensure_evm2(path, profile):
    if path:
        return Path(path).resolve()
    flags = ["--release"] if profile == "release" else []
    log("building evm2 (cargo build -p evm2-cli)")
    subprocess.run(["cargo", "build", "-q", "-p", "evm2-cli", *flags], cwd=ROOT, check=True)
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, check=True, capture_output=True, text=True
    )
    target = Path(json.loads(metadata.stdout)["target_directory"])
    return target / profile / "evm2"


# --- cases


class Case:
    def __init__(self, name, alloc, env, txs, forks, tracers=None, node=True, directory=None, description=""):
        self.name, self.alloc, self.env, self.txs = name, alloc, env, txs
        self.forks, self.tracers, self.node = forks, tracers, node
        self.directory, self.description = directory, description


def load_json(path):
    return json.loads(Path(path).read_text())


def load_cases(cases_dir):
    cases = []
    for directory in sorted(Path(cases_dir).iterdir()):
        if not (directory / "case.json").exists():
            continue
        meta = load_json(directory / "case.json")
        for fork in meta.get("forks", DEFAULT_FORKS):
            if fork not in load_predeploys()["forkOrder"]:
                sys.exit(f"{directory.name}: unknown fork {fork!r} in case.json")
        cases.append(
            Case(
                directory.name,
                load_json(directory / "alloc.json"),
                load_json(directory / "env.json"),
                load_json(directory / "txs.json"),
                meta.get("forks", DEFAULT_FORKS),
                meta.get("tracers"),
                meta.get("node", True),
                directory,
                meta.get("description", ""),
            )
        )
    return cases


@functools.cache
def load_predeploys():
    return load_json(DATA / "predeploys.json")


def fork_index(fork):
    return load_predeploys()["forkOrder"].index(fork)


def with_predeploys(alloc, fork):
    """Adds the system contracts that `evm t8n` needs from `fork` on, unless the alloc has them."""
    out = dict(alloc)
    present = {key.lower() for key in out}
    for item in load_predeploys()["predeploys"]:
        if fork_index(fork) >= fork_index(item["minFork"]):
            out.update({a: acc for a, acc in item["alloc"].items() if a not in present})
    return out


# --- running the tools


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=300)


class Tool:
    """A t8n implementation: geth's `evm` or `evm2`."""

    def __init__(self, name, base_cmd):
        self.name, self.base_cmd = name, base_cmd

    def t8n(self, fork, inputs, out, extra):
        cmd = [
            *self.base_cmd, "t8n",
            "--input.alloc", str(inputs / "alloc.json"),
            "--input.env", str(inputs / "env.json"),
            "--input.txs", str(inputs / "txs.json"),
            "--state.fork", fork,
            "--output.basedir", str(out),
            "--output.result", "result.json",
            "--output.alloc", "alloc.json",
            "--output.body", "body.json",
            *extra,
        ]
        return run(cmd)


def write_inputs(directory, case, alloc):
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "alloc.json").write_text(json.dumps(alloc))
    (directory / "env.json").write_text(json.dumps(case.env))
    (directory / "txs.json").write_text(json.dumps(case.txs))


def read_outputs(out):
    """Reads result, alloc, body and the per transaction traces of one t8n run."""
    outputs = {}
    for name in ("result", "alloc", "body"):
        path = out / f"{name}.json"
        outputs[name] = json.loads(path.read_text()) if path.exists() else None
    traces, files = [], []
    pattern = re.compile(r"trace-(\d+)-(0x[0-9a-f]+)\.(jsonl|json)$")
    for path in sorted(out.glob("trace-*"), key=lambda p: int(pattern.match(p.name).group(1))):
        text = path.read_text()
        files.append([int(pattern.match(path.name).group(1)), pattern.match(path.name).group(2), bool(text.strip())])
        if not text.strip():
            continue  # with --trace, geth opens a file for a rejected transaction and writes nothing
        traces.append([json.loads(line) for line in text.splitlines() if line.strip()] if path.suffix == ".jsonl" else json.loads(text))
    outputs["traces"], outputs["files"] = traces, files
    return outputs


def run_tool(tool, fork, inputs, workdir, tracer):
    """Runs one tool's t8n for one tracer; returns (outputs, error)."""
    out = workdir / tool.name / tracer
    out.mkdir(parents=True, exist_ok=True)
    try:
        proc = tool.t8n(fork, inputs, out, evm2_flags(tracer))
    except subprocess.TimeoutExpired:
        return None, f"{CRASHED}{tool.name} t8n timed out"
    if proc.returncode != 0:
        text = proc.stderr or proc.stdout
        lines = [l for l in text.splitlines() if l.strip()]
        message = " | ".join(lines[-3:])[:600] or f"exit status {proc.returncode}"
        if proc.returncode < 0 or "panicked at" in text or "panic:" in text:
            message = CRASHED + message
        return None, message
    return read_outputs(out), None


# --- struct logger from a geth node


PORT_LOCK = threading.Lock()
NEXT_PORT = [20000 + os.getpid() % 100 * 100]


def free_port():
    """A port no other worker of this run uses and nothing is listening on."""
    with PORT_LOCK:
        while True:
            NEXT_PORT[0] += 1
            if NEXT_PORT[0] > 32000:
                NEXT_PORT[0] = 20001
            with socket.socket() as sock:
                try:
                    sock.bind(("127.0.0.1", NEXT_PORT[0]))
                except OSError:
                    continue
                return NEXT_PORT[0]


def rpc(port, method, params):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}",
        json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        {"content-type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        reply = json.load(response)
    if "error" in reply:
        raise RuntimeError(reply["error"].get("message", reply["error"]))
    return reply["result"]


def rpc_retry(port, method, params, tries=200):
    for _ in range(tries):
        try:
            return rpc(port, method, params)
        except RuntimeError as err:
            if "indexing is in progress" not in str(err) and "not found" not in str(err):
                raise
        time.sleep(0.05)
    raise RuntimeError(f"{method} did not complete")


def rlp_items(data):
    """Splits an RLP list into transactions: typed ones as their payload, legacy ones whole."""

    def header(i):
        first = data[i]
        if first < 0x80:
            return "s", i, i + 1
        if first < 0xB8:
            return "s", i + 1, i + 1 + first - 0x80
        if first < 0xC0:
            size = first - 0xB7
            return "s", i + 1 + size, i + 1 + size + int.from_bytes(data[i + 1 : i + 1 + size], "big")
        if first < 0xF8:
            return "l", i + 1, i + 1 + first - 0xC0
        size = first - 0xF7
        return "l", i + 1 + size, i + 1 + size + int.from_bytes(data[i + 1 : i + 1 + size], "big")

    kind, start, end = header(0)
    assert kind == "l"
    items, i = [], start
    while i < end:
        kind, payload_start, payload_end = header(i)
        items.append(data[i:payload_end] if kind == "l" else data[payload_start:payload_end])
        i = payload_end
    return items


def genesis(case, fork, alloc):
    index = fork_index(fork)

    def active(name):
        return index >= fork_index(name)

    config = {
        "chainId": 1, "homesteadBlock": 0, "eip150Block": 0, "eip155Block": 0, "eip158Block": 0,
        "byzantiumBlock": 0, "constantinopleBlock": 0, "petersburgBlock": 0, "istanbulBlock": 0,
        "berlinBlock": 0, "londonBlock": 0, "mergeNetsplitBlock": 0, "terminalTotalDifficulty": 0,
        "blobSchedule": {
            "cancun": {"target": 3, "max": 6, "baseFeeUpdateFraction": 3338477},
            "prague": {"target": 6, "max": 9, "baseFeeUpdateFraction": 5007716},
            "osaka": {"target": 6, "max": 9, "baseFeeUpdateFraction": 5007716},
        },
    }
    for name in ("Shanghai", "Cancun", "Prague", "Osaka", "Amsterdam"):
        if active(name):
            config[f"{name.lower()}Time"] = 0
    if active("Prague"):
        config["depositContractAddress"] = "0x00000000219ab540356cbb839cbe05303d7705fa"
    wanted = int(case.env.get("currentBaseFee", "0x7"), 16)
    # The first block's base fee is the genesis fee minus an eighth; the fee is near wanted * 8 / 7.
    base_fee = next(b for b in range(wanted * 8 // 7 - 2, wanted * 8 // 7 + 3) if b - b // 8 == wanted)

    def pad_storage(account):
        account = dict(account)
        account["storage"] = {f"0x{int(k, 16):064x}": f"0x{int(v, 16):064x}" for k, v in account.get("storage", {}).items()}
        return account

    return {
        "config": config, "nonce": "0x0", "timestamp": "0x0", "extraData": "0x",
        "gasLimit": case.env["currentGasLimit"], "difficulty": "0x0", "mixHash": "0x" + "00" * 32,
        "baseFeePerGas": hex(base_fee), "alloc": {a: pad_storage(acc) for a, acc in alloc.items()},
    }


NODE_SLOTS = threading.Semaphore(8)


def node_traces(geth_dir, case, fork, alloc, body, workdir, tracers):
    """Runs the body's transactions on a `geth --dev` node of the fork and traces each one with
    every tracer; returns {tracer: [frame per tx]}."""
    node = workdir / "node"
    node.mkdir(parents=True, exist_ok=True)
    (node / "genesis.json").write_text(json.dumps(genesis(case, fork, alloc)))
    init = run([str(geth_dir / "geth"), "--datadir", str(node / "data"), "init", str(node / "genesis.json")])
    if init.returncode != 0:
        return None, "geth init: " + init.stderr.strip().splitlines()[-1]
    port = free_port()
    with NODE_SLOTS:
        with open(node / "geth.log", "w") as log_file:
            proc = subprocess.Popen(
                [
                    str(geth_dir / "geth"), "--dev", "--dev.period", "0", "--datadir", str(node / "data"),
                    "--http", "--http.addr", "127.0.0.1", "--http.port", str(port), "--http.api", "eth,debug",
                    "--miner.gasprice", "1", "--txpool.pricelimit", "1", "--port", "0",
                    "--miner.pending.feeRecipient", case.env["currentCoinbase"],
                    "--authrpc.port", str(free_port()), "--ipcdisable", "--maxpeers", "0", "--cache", "64",
                ],
                stdout=log_file, stderr=log_file,
            )
            try:
                for _ in range(400):
                    try:
                        rpc(port, "eth_chainId", [])
                        break
                    except Exception:
                        if proc.poll() is not None:
                            return None, "geth --dev exited: " + (node / "geth.log").read_text()[-300:]
                        time.sleep(0.05)
                traces = {tracer: [] for tracer in tracers}
                for raw in rlp_items(bytes.fromhex(body[2:])) if body and len(body) > 4 else []:
                    try:
                        tx_hash = rpc(port, "eth_sendRawTransaction", ["0x" + raw.hex()])
                        rpc_retry(port, "eth_getTransactionReceipt", [tx_hash])
                        for tracer in tracers:
                            traces[tracer].append(rpc_retry(port, "debug_traceTransaction", [tx_hash, node_options(tracer)]))
                    except Exception as err:
                        return None, f"node: {err}"
                return traces, None
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()


# --- normalisation and diff


HEX = re.compile(r"^0x[0-9a-fA-F]*$")


def lower_hex(value):
    if isinstance(value, dict):
        return {k: lower_hex(v) for k, v in value.items()}
    if isinstance(value, list):
        return [lower_hex(v) for v in value]
    if isinstance(value, str) and HEX.match(value):
        return value.lower()
    return value


CONTEXT_FIELDS = ("blockHash", "blockNumber", "transactionHash", "transactionPosition")


def normalize_flat(frames):
    """Drops the block and transaction identity, which only the node knows."""
    return [{k: v for k, v in frame.items() if k not in CONTEXT_FIELDS} for frame in frames]


def normalize_trace(tracer, value):
    if tracer == "flat" and isinstance(value, list):
        value = normalize_flat(value)
    return lower_hex(value)


def normalize_result(result, strict_errors):
    result = dict(result or {})
    if not strict_errors and result.get("rejected"):
        result["rejected"] = [{"index": r["index"]} for r in result["rejected"]]
    return lower_hex(result)


def diff(a, b, path=()):
    """Yields (path, a, b) for every difference; dict keys are unordered, lists are not."""
    if isinstance(a, dict) and isinstance(b, dict):
        for key in sorted(set(a) | set(b)):
            if key not in a:
                yield (*path, key), "<missing>", b[key]
            elif key not in b:
                yield (*path, key), a[key], "<missing>"
            else:
                yield from diff(a[key], b[key], (*path, key))
    elif isinstance(a, list) and isinstance(b, list):
        for i in range(max(len(a), len(b))):
            if i >= len(a):
                yield (*path, i), "<missing>", b[i]
            elif i >= len(b):
                yield (*path, i), a[i], "<missing>"
            else:
                yield from diff(a[i], b[i], (*path, i))
    elif type(a) in (int, float) and type(b) in (int, float):
        # Integers compare exactly; a float is a JavaScript number, so it matches the integer it rounds to.
        if (a != b) if type(a) is type(b) is int else (float(a) != float(b)):
            yield path, a, b
    elif a != b or type(a) is not type(b):
        yield path, a, b


def format_path(path):
    out = ""
    for part in path:
        out += f"[{part}]" if isinstance(part, int) else (f".{part}" if out else str(part))
    return out


def collapse_path(path):
    text = re.sub(r"\[\d+\]", "[]", format_path(path))
    text = re.sub(r"0x[0-9a-fA-F]{64}", "<hash>", text)
    return re.sub(r"0x[0-9a-fA-F]{40}", "<address>", text)


def short(value, limit=90):
    text = json.dumps(value, separators=(",", ":")) if not isinstance(value, str) else value
    return text if len(text) <= limit else text[: limit - 3] + "..."


def step_op(path, *trees):
    """The opcode name of the step a difference is in, or "" outside a step; the first tree that has one wins."""
    for node in trees:
        op = ""
        try:
            for part in path:
                node = node[part]
                if isinstance(node, dict):
                    name = node.get("opName", node.get("op"))
                    op = name if isinstance(name, str) else op
        except (KeyError, IndexError, TypeError):
            pass
        if op:
            return op
    return ""


class Difference:
    def __init__(self, case, fork, tracer, tx, path, geth, evm2, op=""):
        self.case, self.fork, self.tracer, self.tx, self.path = case, fork, tracer, tx, path
        self.geth, self.evm2, self.op = geth, evm2, op
        self.collapsed = collapse_path(path)

    def line(self):
        where = f"tx{self.tx} " if self.tx is not None else ""
        return f"{self.case} {self.fork} {self.tracer} {where}{format_path(self.path)}: geth={short(self.geth)} evm2={short(self.evm2)}"


def compare(case, fork, tracer, geth, evm2, strict_errors):
    """Compares normalised outputs of one (case, fork, tracer); returns Differences."""
    found = []
    if tracer == "result":
        for name in ("result", "alloc"):
            norm = (lambda value: normalize_result(value, strict_errors)) if name == "result" else lower_hex
            diffs = diff(norm(geth[name]), norm(evm2[name]))
            found += [Difference(case, fork, tracer, None, (name, *path), x, y) for path, x, y in diffs]
        return found
    g, e = geth["traces"], evm2["traces"]
    if geth.get("files") is not None and geth["files"] != evm2["files"]:  # numbering and hashes of the trace files
        found.append(Difference(case, fork, tracer, None, ("<trace files>",), geth["files"], evm2["files"]))
    if len(g) != len(e):
        found.append(Difference(case, fork, tracer, None, ("<traces>",), len(g), len(e)))
    for tx, (gt, et) in enumerate(zip(g, e)):
        gt, et = normalize_trace(tracer, gt), normalize_trace(tracer, et)
        found += [Difference(case, fork, tracer, tx, path, x, y, step_op(path, gt, et)) for path, x, y in diff(gt, et)]
    return found


# --- known differences


# `op` limits an entry to the steps of the named opcodes; without it the entry covers every difference at its path.
KNOWN_KEYS = ("case", "fork", "tracer", "path", "op")

# `kind` of an entry for something geth does not implement, or an evm2 field geth does not print;
# fix PRs never delete these entries.
GETH_GAP = "geth-gap"
EXTENSION = "extension"
PERMANENT = (GETH_GAP, EXTENSION)


def load_known(path):
    """Reads the known differences, one atom per case, fork, tracer and path.

    An entry's case, fork, tracer, path and op may be a list instead of a string; its atoms are the product.
    """
    entries = load_json(path)["known"] if Path(path).exists() else []
    as_list = lambda value: value if isinstance(value, list) else [value]
    for entry in entries:
        missing = [key for key in KNOWN_KEYS[:4] if key not in entry]
        if missing:
            sys.exit(f"known difference without {', '.join(missing)} (use \"*\" to match all): {json.dumps(entry)[:200]}")
        if all(entry[key] == "*" for key in ("case", "tracer", "path")):
            sys.exit(f"known difference that matches every case, tracer and path: {json.dumps(entry)[:200]}")
    return [
        dict(zip(KNOWN_KEYS, combination), reason=entry.get("reason", ""), kind=entry.get("kind", ""))
        for entry in entries
        for combination in itertools.product(*(as_list(entry.get(key, "*")) for key in KNOWN_KEYS))
    ]


@functools.lru_cache(maxsize=None)
def glob_regex(pattern):
    return re.compile(".*".join(re.escape(part) for part in pattern.split("*")))


def glob(pattern, text):
    """`*` matches any text; every other character, including `[` and `]`, matches itself."""
    return glob_regex(pattern).fullmatch(text) is not None


def atom_matches(atom, d):
    return (
        glob(atom["case"], d.case) and glob(atom["fork"], d.fork)
        and glob(atom["tracer"], d.tracer) and glob(atom["path"], d.collapsed) and glob(atom["op"], d.op)
    )


def atom_selected(atom, ran):
    """Whether the run covered a (case, fork, tracer) that `atom` selects."""
    return any(glob(atom["case"], c) and glob(atom["fork"], f) and glob(atom["tracer"], t) for c, f, t in ran)


# --- EEST state tests


def eest_cases(path, patterns, forks):
    path = Path(path)
    files = [path] if path.is_file() else sorted(path.rglob("*.json"))
    root = path if path.is_dir() else path.parent
    for file in files:
        try:
            suite = json.loads(file.read_text())
        except Exception:
            continue
        for test, unit in suite.items():
            if not isinstance(unit, dict) or "post" not in unit or "transaction" not in unit:
                continue
            for fork, posts in unit["post"].items():
                if fork not in load_predeploys()["forkOrder"] or (forks and fork not in forks):
                    continue
                for post in posts:
                    name = f"{file.relative_to(root)}::{test}::{fork}::{post['indexes']['data']}-{post['indexes']['gas']}-{post['indexes']['value']}"
                    if matches(name, patterns) and (case := eest_case(name, unit, fork, post["indexes"])):
                        yield case


def quantity(value):
    """Rewrites a hex quantity without leading zeros, as geth's transaction decoder demands."""
    return hex(int(value, 16)) if isinstance(value, str) and value.startswith("0x") and len(value) > 2 else value


def eest_case(name, unit, fork, indexes):
    env = unit["env"]
    if env.get("currentChainID") not in (None, "0x1", "0x01"):
        return None
    tx = unit["transaction"]
    if not tx.get("secretKey"):
        return None
    out = {
        "nonce": quantity(tx["nonce"]),
        "gas": quantity(tx["gasLimit"][indexes["gas"]]),
        "input": tx["data"][indexes["data"]],
        "value": quantity(tx["value"][indexes["value"]]),
        "v": "0x0", "r": "0x0", "s": "0x0",
        "secretKey": tx["secretKey"],
    }
    if tx.get("to"):
        out["to"] = tx["to"]
    access = tx["accessLists"][indexes["data"]] if tx.get("accessLists") else None
    if "maxFeePerGas" in tx:
        out.update(maxFeePerGas=quantity(tx["maxFeePerGas"]), maxPriorityFeePerGas=quantity(tx.get("maxPriorityFeePerGas", "0x0")))
    elif "gasPrice" in tx:
        out["gasPrice"] = quantity(tx["gasPrice"])
    ty = tx.get("type")
    if ty is None:
        ty = 4 if "authorizationList" in tx else 3 if "blobVersionedHashes" in tx else 2 if "maxFeePerGas" in tx else 1 if access is not None else 0
    ty = int(ty, 16) if isinstance(ty, str) else ty
    if ty:
        out["type"] = hex(ty)
        out["chainId"] = quantity(tx.get("chainId", "0x1"))
        out["accessList"] = access or []
    if ty == 3:
        out["blobVersionedHashes"] = tx["blobVersionedHashes"]
        out["maxFeePerBlobGas"] = quantity(tx["maxFeePerBlobGas"])
    if ty == 4:
        out["authorizationList"] = [
            {"chainId": quantity(a["chainId"]), "address": a["address"], "nonce": quantity(a["nonce"]), "yParity": quantity(a.get("yParity", a.get("v"))), "r": quantity(a["r"]), "s": quantity(a["s"])}
            for a in tx["authorizationList"]
        ]
    if ty == 0 and fork_index(fork) < fork_index("EIP158"):
        out["protected"] = False  # EIP-155 replay protection starts with Spurious Dragon
    number = int(env["currentNumber"], 16)
    t8n_env = {
        "currentCoinbase": env["currentCoinbase"], "currentGasLimit": env["currentGasLimit"],
        "currentNumber": env["currentNumber"], "currentTimestamp": env["currentTimestamp"],
        "currentDifficulty": env.get("currentDifficulty", "0x0"),
    }
    for key in ("currentBaseFee", "currentRandom", "currentExcessBlobGas", "slotNumber"):
        if env.get(key) is not None:
            t8n_env[key] = env[key]
    t8n_env["blockHashes"] = {str(n): "0x" + hashlib.sha256(str(n).encode()).hexdigest() for n in range(max(0, number - 256), number)}
    if fork_index(fork) >= fork_index("Shanghai"):
        t8n_env["withdrawals"] = []
    if fork_index(fork) >= fork_index("Cancun"):
        t8n_env["parentBeaconBlockRoot"] = "0x" + "00" * 32
    if fork_index(fork) >= fork_index("Merge") and "currentRandom" not in t8n_env:
        t8n_env["currentRandom"] = "0x" + "00" * 32
    pre = {a: {"balance": acc["balance"], "code": acc["code"], "nonce": acc["nonce"], "storage": acc.get("storage", {})} for a, acc in unit["pre"].items()}
    return Case(name, pre, t8n_env, [out], [fork], node=False, description="EEST state test")


# --- main run


def matches(name, patterns):
    """Whether `name` matches one of the glob or substring patterns; no patterns match all."""
    return not patterns or any(fnmatch.fnmatchcase(name, p) or p in name for p in patterns)


def resolve_selection(cases, patterns):
    return [c for c in cases if matches(c.name, patterns)]


def job_list(cases, args):
    """The (case, fork, tracer) comparisons to run."""
    default = ["result"] if args.eest else DEFAULT_TRACERS
    jobs = []
    for case in cases:
        for fork in case.forks:
            if args.fork and fork not in args.fork:
                continue
            node_ok = case.node and fork_index(fork) >= fork_index(NODE_MIN_FORK) and not (args.no_node or args.eest)
            for tracer in args.tracer or default:
                wanted = args.tracer or case.tracers is None or tracer in case.tracers or tracer == "result"
                if wanted and (tracer in T8N_TRACERS or node_ok):
                    jobs.append((case, fork, tracer))
    return jobs


def expected_path(case, fork):
    return case.directory / "expected" / f"{fork}.json"


def stored_outputs(case, fork, tracer):
    path = expected_path(case, fork)
    if not path.exists():
        return None
    bundle = load_json(path)
    if tracer == "result":
        return {"result": bundle["result"], "alloc": bundle["alloc"]}
    if tracer not in bundle["traces"]:
        return None
    return {"traces": bundle["traces"][tracer], "files": bundle.get("files") if tracer == "jsonl" else None}


def write_bundle(case, fork, geth_runs):
    path = expected_path(case, fork)
    bundle = load_json(path) if path.exists() else {"traces": {}}
    if "result" not in bundle and "result" not in geth_runs:
        log(f"not blessed: {case.name} {fork}: a new bundle needs the result tracer")
        return
    bundle["geth"] = GETH_VERSION
    for tracer, outputs in geth_runs.items():
        if tracer == "result":
            bundle["result"], bundle["alloc"] = lower_hex(outputs["result"]), lower_hex(outputs["alloc"])
        else:
            bundle["traces"][tracer] = [normalize_trace(tracer, t) for t in outputs["traces"]]
            if tracer == "jsonl":
                bundle["files"] = outputs["files"]
    path.parent.mkdir(exist_ok=True)
    dump = lambda value: json.dumps(value, separators=(",", ":"), sort_keys=True)
    traces = ",\n".join(f"  {json.dumps(t)}: {dump(v)}" for t, v in sorted(bundle["traces"].items()))
    fields = [f'"{key}": {dump(bundle[key])}' for key in ("alloc", "files", "geth", "result") if key in bundle]
    path.write_text("{\n" + ",\n".join([*fields, f'"traces": {{\n{traces}\n}}']) + "\n}\n")


def process(args, tools, case, fork, jobs, workdir):
    """Runs all selected tracers of one (case, fork); returns (differences, errors, geth_runs)."""
    found, errors, geth_runs = [], [], {}
    inputs = workdir / "inputs"
    alloc = with_predeploys(case.alloc, fork)
    write_inputs(inputs, case, alloc)
    node_ids = [t for t in jobs if t not in T8N_TRACERS]
    node_out, geth_result = {}, None
    if node_ids and not args.offline:
        geth_result, err = run_tool(tools["geth"], fork, inputs, workdir, "result")
        if not err:
            node_out, err = node_traces(tools["geth_dir"], case, fork, alloc, geth_result["body"], workdir, node_ids)
        if err:
            return found, [(case.name, fork, t, f"geth: {err}") for t in node_ids], geth_runs
    for tracer in jobs:
        evm2_out, evm2_err = run_tool(tools["evm2"], fork, inputs, workdir, tracer)
        geth_err = None
        if args.offline:
            geth_out = stored_outputs(case, fork, tracer)
            if geth_out is None:
                errors.append((case.name, fork, tracer, "no stored geth output; run with --bless"))
                continue
        elif tracer == "result" and geth_result:
            geth_out = geth_result
        elif tracer in T8N_TRACERS:
            geth_out, geth_err = run_tool(tools["geth"], fork, inputs, workdir, tracer)
        else:
            geth_out = {"traces": node_out[tracer]}
        if evm2_err and geth_err and args.eest and CRASHED not in evm2_err + geth_err:
            errors.append((case.name, fork, tracer, f"{REJECTED} (geth: {geth_err}; evm2: {evm2_err})"))
        elif evm2_err and geth_err:
            errors.append((case.name, fork, tracer, f"geth: {geth_err}; evm2: {evm2_err}"))
        elif evm2_err or geth_err:
            errors.append((case.name, fork, tracer, f"geth: {geth_err}" if geth_err else f"evm2: {evm2_err}"))
        else:
            geth_runs[tracer] = geth_out
            found += compare(case.name, fork, tracer, geth_out, evm2_out, args.strict_errors)
    return found, errors, geth_runs


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cases", nargs="*", metavar="CASE", help="case name substrings or globs")
    parser.add_argument("--cases-dir", default=str(DATA / "cases"), help="directory of cases")
    parser.add_argument("--fork", action="append", help="only this fork (repeatable)")
    parser.add_argument("--tracer", action="append", help="only this tracer id (repeatable)")
    parser.add_argument("--geth-dir", metavar="DIR", help="directory with prebuilt geth `evm` and `geth` (default: build the pinned commit)")
    parser.add_argument("--evm2", help="evm2 binary (default: cargo build -p evm2-cli)")
    parser.add_argument("--profile", default="debug", choices=["debug", "release"], help="cargo profile to build")
    parser.add_argument("--offline", action="store_true", help="compare evm2 with the stored geth outputs, without geth")
    parser.add_argument("--bless", action="store_true", help="store geth's outputs as case/expected/<Fork>.json")
    parser.add_argument("--no-node", action="store_true", help="skip the tracers that need the geth node")
    parser.add_argument("--known-diffs", default=str(DATA / "known-diffs.json"), help="known differences file")
    parser.add_argument("--write-known-diffs", action="store_true", help="rewrite the known differences file from this run")
    parser.add_argument("--strict-errors", action="store_true", help="also compare messages of rejected transactions")
    parser.add_argument("--keep", metavar="DIR", help="keep raw outputs of both tools in DIR")
    parser.add_argument("--js-dir", help="also run each *.js file of DIR as tracer js:<name>")
    parser.add_argument("--eest", metavar="PATH", help="check EEST state test fixtures (file or directory)")
    parser.add_argument("--max-diffs", type=int, default=8, help="differences printed per case, fork and tracer")
    parser.add_argument("-j", "--jobs", type=int, default=os.cpu_count(), help="parallel jobs")
    parser.add_argument("--list", action="store_true", help="list cases, forks and tracers, then exit")
    args = parser.parse_args()

    if args.js_dir:
        for p in sorted(Path(args.js_dir).glob("*.js")):
            TRACER_SPECS[f"js:{p.stem}"] = {"kind": "tracer", "js": str(p.resolve()), "config": {}}
            DEFAULT_TRACERS.append(f"js:{p.stem}")
    for t in args.tracer or []:
        if t not in TRACER_SPECS:
            sys.exit(f"unknown tracer {t}; known: {', '.join(TRACER_SPECS)}")
    if args.eest:
        if args.bless:
            sys.exit("--bless stores outputs next to committed cases; it does not apply to --eest")
        cases = list(eest_cases(args.eest, args.cases, args.fork))
    else:
        cases = resolve_selection(load_cases(args.cases_dir), args.cases)
    if args.list:
        for case in cases:
            print(f"{case.name}: forks={','.join(case.forks)} tracers={','.join(case.tracers or ['*'])} node={case.node}  {case.description}")
        print(f"tracers: {', '.join(TRACER_SPECS)}")
        return 0
    if not cases:
        sys.exit("no case matches")

    geth_dir = None if args.offline else ensure_geth(args.geth_dir)
    evm2 = ensure_evm2(args.evm2, args.profile)
    tools = {"evm2": Tool("evm2", [str(evm2)]), "geth_dir": geth_dir}
    if geth_dir:
        tools["geth"] = Tool("geth", [str(geth_dir / "evm")])

    jobs = job_list(cases, args)
    if not jobs:
        sys.exit("no case, fork and tracer combination selected")
    by_pair = {}
    for case, fork, tracer in jobs:
        by_pair.setdefault((case.name, fork), (case, []))[1].append(tracer)
    order = {pair: n for n, pair in enumerate(by_pair)}
    workroot = Path(args.keep).resolve() if args.keep else Path(tempfile.mkdtemp(prefix="geth-parity-"))
    workroot.mkdir(parents=True, exist_ok=True)

    log(f"{len(by_pair)} case/fork pairs, {len(jobs)} comparisons, geth {GETH_VERSION}{' (offline)' if args.offline else ''}")
    known = load_known(args.known_diffs)
    differences, errors, done = [], [], 0
    started = time.time()

    def work(item):
        (name, fork), (case, tracers) = item
        directory = workroot / f"{order[name, fork]:06}-{re.sub(r'[^A-Za-z0-9_.-]', '_', name)[-60:]}" / fork
        if directory.exists():
            shutil.rmtree(directory)
        found, errs, geth_runs = process(args, tools, case, fork, tracers, directory)
        if args.bless:
            if errs:
                log(f"not blessed: {case.name} {fork}: {errs[0][3]}")
            else:
                write_bundle(case, fork, geth_runs)
        return found, errs

    pool = futures.ThreadPoolExecutor(max_workers=max(1, args.jobs))
    try:
        for found, errs in pool.map(work, by_pair.items()):
            differences += found
            errors += errs
            done += 1
            if done % max(25, len(by_pair) // 20) == 0:
                log(f"  {done}/{len(by_pair)} pairs ({time.time() - started:.0f}s)")
    finally:
        pool.shutdown(wait=True, cancel_futures=True)
    if not args.keep:
        shutil.rmtree(workroot, ignore_errors=True)

    return report(args, differences, errors, known, [(c.name, f, t) for c, f, t in jobs], len(by_pair))


def report(args, differences, errors, known, ran, pairs):
    unknown, hits, kinds = [], set(), {GETH_GAP: 0, EXTENSION: 0}
    # Atoms without a wildcard in tracer and path are found by that pair; the others are tried for every difference.
    by_path, wild = {}, []
    for i, atom in enumerate(known):
        if "*" in atom["tracer"] or "*" in atom["path"]:
            wild.append(i)
        else:
            by_path.setdefault((atom["tracer"], atom["path"]), []).append(i)
    for d in differences:
        candidates = [*by_path.get((d.tracer, d.collapsed), ()), *wild]
        matched = [i for i in candidates if atom_matches(known[i], d)]
        hits.update(matched)
        for kind in kinds:
            kinds[kind] += any(known[i]["kind"] == kind for i in matched)
        if not matched:
            unknown.append(d)
    if args.write_known_diffs:
        if errors:
            sys.exit(f"not rewriting the known differences: {len(errors)} runs failed, first: {errors[0]}")
        write_known(args.known_diffs, differences, known, ran)
        log(f"wrote {args.known_diffs}")
        return 0
    stale = [atom for i, atom in enumerate(known) if i not in hits and atom_selected(atom, ran)]

    unclassified = {(a["case"], a["tracer"], a["path"]) for a in known if len(a["reason"].strip()) < 10 or a["reason"] == "unclassified"}
    for case, tracer, path in sorted(unclassified)[:20]:
        print(f"UNCLASSIFIED known diff needs a reason: {case} {tracer} {path}")

    rejected = [e for e in errors if e[3].startswith(REJECTED)]
    errors = [e for e in errors if not e[3].startswith(REJECTED)]
    groups = {}
    for d in unknown:
        groups.setdefault((d.case, d.fork, d.tracer), []).append(d)
    for (case, fork, tracer), items in groups.items():
        for d in items[: args.max_diffs]:
            print(d.line())
        if len(items) > args.max_diffs:
            print(f"{case} {fork} {tracer}: ... {len(items) - args.max_diffs} more differences")
    for case, fork, tracer, message in errors:
        print(f"ERROR {case} {fork} {tracer}: {message}")
    for atom in stale[:50]:
        print(f"STALE known diff no longer occurs, delete it: {atom['case']} {atom['fork']} {atom['tracer']} {atom['path']}")
    if len(stale) > 50:
        print(f"STALE ... {len(stale) - 50} more")

    total = len(differences)
    print(
        f"\n{pairs} case/fork pairs, {len(ran)} comparisons: {total} differences "
        f"({total - len(unknown) - sum(kinds.values())} known, {kinds[GETH_GAP]} geth gaps, {kinds[EXTENSION]} extensions, {len(unknown)} new), {len(errors)} errors, {len(stale)} stale known entries, "
        f"{len(rejected)} skipped because both tools reject the input"
    )
    if unknown:
        by_path = {}
        for d in unknown:
            by_path.setdefault((d.tracer, d.collapsed), set()).add(d.case)
        print("new differences by tracer and field:")
        for (tracer, path), names in sorted(by_path.items()):
            print(f"  {tracer:14} {path}: {len(names)} cases")
    return 1 if unknown or errors or stale or unclassified else 0


def write_known(path, differences, old, ran):
    """Rewrites the known file from this run's differences.

    Entries outside the run's selection stay. Reasons carry over by tracer and path; entries
    group by tracer and path, then by the forks and opcodes each case differs in.
    """
    reasons = {(a["tracer"], a["path"]): a["reason"] for a in old if a["reason"] and a["kind"] not in PERMANENT}
    permanent = [a for a in old if a["kind"] in PERMANENT]
    found = {}
    for d in differences:
        if any(atom_matches(a, d) for a in permanent):
            continue
        found.setdefault((d.tracer, d.collapsed), {}).setdefault(d.case, {}).setdefault(d.fork, set()).add(d.op)
    entries = [
        {k: a[k] for k in (*KNOWN_KEYS, "reason", "kind") if a[k] and not (k == "op" and a[k] == "*")}
        for a in old
        if a["kind"] in PERMANENT or not atom_selected(a, ran)
    ]
    for (tracer, collapsed), cases in sorted(found.items()):
        # Cases that differ in the same opcodes at the same forks share an entry, so each entry is an exact product.
        groups = {}
        for case, by_fork in sorted(cases.items()):
            by_ops = {}
            for fork, ops in by_fork.items():
                by_ops.setdefault(frozenset(ops), []).append(fork)
            shape = frozenset((ops, tuple(sorted(forks, key=fork_index))) for ops, forks in by_ops.items())
            groups.setdefault(shape, []).append(case)
        for shape, names in groups.items():
            for ops, forks in sorted(shape, key=lambda item: (item[1], sorted(item[0]))):
                entry = {
                    "case": names if len(names) > 1 else names[0],
                    "fork": list(forks) if len(forks) > 1 else forks[0],
                    "tracer": tracer, "path": collapsed,
                }
                if "" not in ops:  # a difference outside a step stays unpinned
                    entry["op"] = sorted(ops) if len(ops) > 1 else next(iter(ops))
                entry["reason"] = reasons.get((tracer, collapsed)) or "unclassified"
                entries.append(entry)
    Path(path).write_text('{\n  "known": [\n' + ",\n".join("    " + json.dumps(e) for e in entries) + "\n  ]\n}\n")


if __name__ == "__main__":
    sys.exit(main())
