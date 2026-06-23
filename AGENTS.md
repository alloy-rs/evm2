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

## Temporary Branch Context

This branch is porting `paradigmxyz/revmc` into `crates/jit` as the `evm2-jit`
crate while keeping the port's diff against upstream as small and intentional as
possible. The tracked upstream revision is recorded in `diffs/TODO.md`.

The `diffs/` directory tracks generated unified diffs between upstream `revmc`
and the local `crates/jit` port. After every source change that affects the JIT
port, run `./scripts/generate_jit_diffs.sh` and inspect the relevant file in
`diffs/`. Use those generated diffs while editing: if a hunk only reflects
formatting, stale naming, import ordering, or another unnecessary divergence from
upstream, fix the source instead of accepting the diff.

`diffs/TODO.md` is a user-owned manual review checklist generated from the
non-empty diff files. Do not edit or regenerate that checklist unless the user
explicitly tells you to.

Do not add new comments, docs, or identifiers that compare behavior to revm.
Describe behavior self-containedly. Existing comments and crate references
inherited from upstream `revmc` may remain while the port is being minimized and
adapted.

## Commands

```bash
cargo cl # lint
cargo fmt --all # format
cargo docs # check docs

cargo nextest run # test (default filter)
cargo nextest run -E "not test(glob*)" # further filter tests
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
By default it downloads EEST develop (or stable with `EVM2_STATETEST_STABLE=1`)
and legacy Cancun/Constantinople state tests. Devnet fixtures are opt-in with
`DEVNET_VERSION` and `DEVNET_TAR` (downloaded from the `ethereum/execution-specs`
repo, overridable via `DEVNET_BASE_URL`); add `EVM2_STATETEST_DEVNET_ONLY=1` to
skip main/legacy fixtures. Use `EVM2_STATETEST_ROOT` or `EVM2_BLOCKCHAINTEST_ROOT`
for a single explicit root.

To run additional tests from an arbitrary folder (or single file) without a
test-name filter, use `./scripts/eest.sh <path>`. Every JSON file found anywhere
under the path runs as one suite whose kind (state vs blockchain) is detected
per file, applying the same skip lists as the default suites. Fixtures this
runner cannot execute (transaction tests, engine/sync blockchain variants)
therefore surface as failures. The path may be outside the repo. The script just
sets `EVM2_ADDITIONAL_TESTS` (honored by the `eest` harness) and runs `cargo
nextest run -p evm2-eest --test eest --ignore-default-filter`; extra args are
forwarded to nextest.
