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
cargo nextest run --ignore-default-filters # include statetests
```
