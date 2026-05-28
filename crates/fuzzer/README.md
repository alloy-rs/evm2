# evm2-fuzzer

Differential fuzzer for evm2 against revm.

The fuzzer generates structured EVM cases, runs them against revm first as the
oracle, then runs evm2 and compares normalized receipts, logs, errors, gas, and
state changes.

Run generated cases:

```sh
cargo run -q -p evm2-fuzzer -- --seed 1 --cases 100000 -j 0
```

Run by duration with a random seed:

```sh
cargo run -q -p evm2-fuzzer -- --duration 5m -j 0
```

`-j`/`--threads` controls worker threads. `0` uses logical cores. The fuzzer
prints the seed for replayability.

Force an evm2 dispatch backend with `EVM2_DISPATCH_BACKEND`:

```sh
EVM2_DISPATCH_BACKEND=packed cargo run -q -p evm2-fuzzer -- --duration 1m -j 0
```

Accepted backends are `auto`, `tco`, `packed`, `single_return`, and `unpacked`.

## Corpus and replay

Failing generated cases are written to:

```text
crates/fuzzer/corpus/failures/
```

Replay one saved case:

```sh
cargo run -q -p evm2-fuzzer -- replay crates/fuzzer/corpus/failures/case-....json
```

Replay every JSON case in a corpus directory:

```sh
cargo run -q -p evm2-fuzzer -- corpus crates/fuzzer/corpus/failures
```

Minimize a reproducing case:

```sh
cargo run -q -p evm2-fuzzer -- minimize crates/fuzzer/corpus/failures/case-....json
```

## Coverage report

Generate an HTML coverage report from fuzzer execution, excluding the fuzzer
crate itself:

```sh
./scripts/fuzzer_coverage.py --duration 3m -j 0 --open
```

For a smaller report:

```sh
./scripts/fuzzer_coverage.py --cases 1000 --backend packed --open
```

Use `--all-backends` to merge coverage from all explicit dispatch backends.
