# Rust EVM

This repo is a reimplementation of revm. When implementing EVM behavior, use
`bluealloy/revm` as the baseline reference and preserve revm semantics,
control flow, gas accounting, and host interaction shape as closely as possible unless
explicitly told otherwise.

This is a work-in-progress repo with no public API stability guarantees. Do not add
backwards-compatibility aliases, deprecated wrappers, compatibility shims, or similar
transitional API layers unless explicitly requested.

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
`DEVNET_VERSION` and `DEVNET_TAR`; add `EVM2_STATETEST_DEVNET_ONLY=1` to skip
main/legacy fixtures. Use `EVM2_STATETEST_ROOT` or `EVM2_BLOCKCHAINTEST_ROOT`
for a single explicit root.
