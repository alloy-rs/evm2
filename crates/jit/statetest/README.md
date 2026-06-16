# evm2-jit-statetest

[Ethereum state test][tests] runner for evm2-jit.

Runs the official [GeneralStateTests][tests] against three execution modes:

The runner is vendored from revm's `revme` with evm2-jit-specific extensions
for compilation, custom handler integration, and diagnostic diffing between
interpreter and compiled execution.

## Usage

This crate is not published and is used internally by `evm2-jit` (state test integration tests)
and `evm2-jit-cli` (the `statetest` and `statetest-diff` subcommands).

See the [main README](/README.md#testing) for instructions on running state tests.

[tests]: https://github.com/ethereum/tests
