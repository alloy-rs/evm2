# Rust EVM

This repo is a reimplementation of revm. When implementing EVM behavior, use
`~/github/danipopes/revm` as the baseline reference and preserve revm semantics,
control flow, gas accounting, and host interaction shape as closely as possible unless
explicitly told otherwise.

## Commands

```bash
cargo cl                                                      # lint
cargo fmt --all                                               # format
cargo docs                                                    # check docs

cargo nextest run --workspace                                 # test all
cargo nextest run --workspace "test_name"                     # test single
cargo nextest run --workspace "statetest"                     # test statetests
SUBDIR=stRevertTest cargo nextest run --workspace "statetest" # test single statetest
```
