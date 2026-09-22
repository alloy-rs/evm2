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
`tests-glamsterdam-devnet@v8.1.4` / `fixtures_glamsterdam-devnet.tar.gz`
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

## Rust

- Generally add new Rust functions, methods, `impl` blocks, modules, imports, Cargo dependencies, and other items at the bottom of the relevant scope, section, or group. Constructors usually go at the top of an `impl` block. First check where similar items sit in the file and match the existing order, grouping, and style, including alphabetical order where used.
- Put doc comments before attributes, always: `/// ...` comes before `#[derive]`, `#[inline]`, `#[cfg]`, and every other attribute.
- Put module documentation at the top of the module file with inner doc comments (`//! ...`), not on the `mod` item in the parent module.
- NEVER put imports inside functions unless required for `#[cfg(...)]` gating. All imports go at the top of the file.
- Group all `use` imports together. Keep `pub use` imports in a separate group. For local module re-exports, write `mod x;` before `pub use x;`; for re-exporting another module or external crate, use `use x;`, then a blank line, then `pub use y;`, then a blank line before local `mod my_mod; pub use my_mod::*;`.
- Move ordinary test-only imports into the `#[cfg(test)] mod tests` module instead of gating them individually. Keep crate-level dependency anchors such as `#[cfg(test)] use cc as _;` at crate scope.
- In test modules, always import the parent module with `use super::*`.
- In `Cargo.toml`, generally group optional dependencies for a feature together. Put a comment immediately above the group containing only the feature name, for example `# jit`.
- Prefer `let Some(x) = x else { return };` / `let Ok(x) = x else { return };` over `match x { Some(x) => x, _ => return }`.
- Use `let ... else` only for a single early-exit guard. When multiple conditions or patterns gate the same block, prefer a combined `if let` / `let` chain instead of several sequential `let ... else` statements.
- Use combined `if let` chains (`if let Some(x) = x && let Some(y) = y { ... }`) instead of nesting (`if let Some(x) = x { if let Some(y) = y { ... } }`).
- In loops, prefer an `if let` chain around the loop body over multiple `let ... else { continue };` statements when the body only runs if all patterns match.
- NEVER use `ref` / `ref mut` in patterns as the first resort. Always prefer borrowing the expression with `&` / `&mut` instead.
- Prefer map entry APIs such as `entry`, `or_insert`, and `or_insert_with` when multiple consecutive operations would otherwise look up and then insert or update the same key.
- Avoid specifying type hints in variables unless absolutely necessary (e.g. `HashMap<_, Vec<_>>` for `x.entry(y).or_default().push(z)` where type inference won't work). Rely on the compiler.
- When type hints are needed, prefer turbofish (`let x = Type::<X, Y>::new()`) over annotation (`let x: Type<X, Y> = Type::new()`).
- In tests, avoid `.contains` assertions for error/output strings when the project has snapshot testing support such as `snapbox`. Prefer exact snapshot assertions (`stderr_eq`, `stdout_eq`, `assert_data_eq!`, etc.) and use redactions only for genuinely variable parts.
- Always leave a blank line in between module doc-comments, items or item categories, unless in rare exceptions: it's a one-shot struct with one single impl block, or it's a list of impls that are all very similar. But in general blank line in between items is the norm but it's just unenforced. Items includes imports (together) too. The previous rules apply first.
