# Rust EVM

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
