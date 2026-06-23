# Rust EVM

This repo is a reimplementation of revm. When implementing EVM behavior, use
`bluealloy/revm` as the baseline reference and preserve revm semantics,
control flow, gas accounting, and host interaction shape as closely as possible unless
explicitly told otherwise.

This is a work-in-progress repo with no public API stability guarantees. Do not add
backwards-compatibility aliases, deprecated wrappers, compatibility shims, or similar
transitional API layers unless explicitly requested.

## Commands

```bash
cargo cl # lint
cargo fmt --all # format
cargo docs # check docs

cargo nextest run # test (default filter)
cargo nextest run -E "not (test(glob*)) | package(/regex.*/)" # further filter tests
cargo nextest run -p evm2-eest --test eest --ignore-default-filter # include EEST fixtures
```

Use `EVM2_DISPATCH_BACKEND` to force an interpreter dispatch backend for manual
testing. Accepted values are `auto` (default), `tco`, `packed`, `single_return`,
and `unpacked`, for example:

```bash
EVM2_DISPATCH_BACKEND=packed cargo nextest run -p evm2-eest --test eest --ignore-default-filter
```

## EEST Fixtures

`./scripts/setup_test_fixtures.py` downloads fixtures into `test-fixtures/`.
If fixtures are already available in another worktree, symlink `test-fixtures`
to that directory instead of re-downloading them.
By default it downloads EEST develop (or stable with `EVM2_STATETEST_STABLE=1`)
and legacy Cancun/Constantinople state tests. Devnet fixtures are opt-in with
`DEVNET_VERSION` and `DEVNET_TAR` (downloaded from the `ethereum/execution-specs`
repo, overridable via `DEVNET_BASE_URL`); add `EVM2_STATETEST_DEVNET_ONLY=1` to
skip main/legacy fixtures. Use `EVM2_STATETEST_ROOT` or `EVM2_BLOCKCHAINTEST_ROOT`
for a single explicit root.

To run an arbitrary folder (or single file) of fixtures without a test-name
filter, use `./scripts/eest.sh <path>`. Every JSON file found anywhere under the
path runs as one suite whose kind (state vs blockchain) is detected per file;
nothing is skipped, so fixtures this runner cannot execute (transaction tests,
engine/sync blockchain variants) surface as failures. The path may be outside
the repo. The script just sets `EVM2_FIXTURE_PATH` (honored by the `eest`
harness) and runs `cargo nextest run -p evm2-eest --test eest
--ignore-default-filter`; extra args are forwarded to nextest.
