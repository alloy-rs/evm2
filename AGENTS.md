# Rust EVM

This repo is a reimplementation of revm. When implementing EVM behavior, use
`bluealloy/revm` as the baseline reference and preserve revm semantics,
control flow, gas accounting, and host interaction shape as closely as possible unless
explicitly told otherwise.

This is a work-in-progress repo with no public API stability guarantees. Do not add
backwards-compatibility aliases, deprecated wrappers, compatibility shims, or similar
transitional API layers unless explicitly requested.

For all work under `crates/jit`, follow `crates/jit/AGENTS.md` in addition to
this root file.

## Commands

```bash
cargo cl # lint
cargo fmt --all # format
cargo docs # check docs

cargo nextest run # test (default filter)
cargo nextest run -E "not (test(glob*)) | package(/regex.*/)" # further filter tests
cargo nextest run -p evm2-eest --test eest --ignore-default-filter # include EEST fixtures
cargo st # include EEST fixtures with interpreter, JIT, and AOT suites
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
By default it downloads the glamsterdam (Amsterdam) devnet fixtures plus legacy
Cancun/Constantinople state tests. The devnet release defaults to
`tests-glamsterdam-devnet@v6.1.1` / `fixtures_glamsterdam-devnet.tar.gz`
(from the `ethereum/execution-specs` repo, base overridable via `DEVNET_BASE_URL`);
override `DEVNET_VERSION` and `DEVNET_TAR` to select a different devnet release, or
clear either to disable devnet. The glamsterdam devnet fixtures cover
frontier..amsterdam, so the EEST `main` develop suite is now opt-in via
`EVM2_STATETEST_MAIN=1` (stable with `EVM2_STATETEST_STABLE=1`). Add
`EVM2_STATETEST_DEVNET_ONLY=1` to skip legacy fixtures. Use `EVM2_STATETEST_ROOT`
or `EVM2_BLOCKCHAINTEST_ROOT` for a single explicit root.

Compiled EEST runs use a default subset when the full corpus is too expensive:
AOT defaults to the `ci-aot` subset, and JIT defaults to the `ci-smoke` subset.
Set `EVM2_COMPILED_EEST_SUBSET=all` to force full coverage, or set it to
`ci-aot` or `ci-smoke` to run those subsets explicitly.

To run additional tests from an arbitrary folder (or single file) without a
test-name filter, use `./scripts/eest.sh <path>`. Every JSON file found anywhere
under the path runs as one suite whose kind (state vs blockchain) is detected
per file, applying the same skip lists as the default suites. Fixtures this
runner cannot execute (transaction tests, engine/sync blockchain variants)
therefore surface as failures. The path may be outside the repo. The script just
sets `EVM2_ADDITIONAL_TESTS` (honored by the `eest` harness) and runs `cargo
nextest run -p evm2-eest --test eest --ignore-default-filter`; extra args are
forwarded to nextest.
