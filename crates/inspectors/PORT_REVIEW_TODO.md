# revm-inspectors Port Review Todo

Baseline: `/home/doni/github/paradigmxyz/revm-inspectors`.

Port: `crates/inspectors`.

Scope: every original Rust source file under `src/` and every original Rust integration test file
under `tests/it/`. Non-Rust testdata and writer snapshot files are tracked under fixture coverage.

Expected port-wide differences:
- `revm` context/interpreter/database types are replaced by `evm2` types.
- `Inspector::log_full`, `frame_start`, and `frame_end` hooks are absent in the `evm2` inspector API.
- Imports, formatting, docs, and test harness setup may differ.
- Deprecated upstream APIs may stay omitted in this work-in-progress crate.

Review status legend:
- `[ ]`: not reviewed in the current pass.
- `[x]`: reviewed in the current pass.

## Inventory

- Upstream Rust source files: 23.
- Ported Rust source files: 22.
- Missing source files: `src/edge_cov.rs` only; intentionally omitted.
- Upstream Rust integration test files: 12.
- Ported Rust integration test files: 11.
- Missing integration test files: `tests/it/edge_cov.rs` only; intentionally omitted.

## Source Files

- [ ] `src/lib.rs`
  - Pending round-2 review.

- [ ] `src/access_list.rs`
  - Pending round-2 review with `tests/it/accesslist.rs`.

- [ ] `src/edge_cov.rs`
  - Intentionally omitted from the evm2 port.

- [ ] `src/opcode.rs`
  - Pending round-2 review.

- [ ] `src/storage.rs`
  - Pending round-2 review.

- [ ] `src/transfer.rs`
  - Pending round-2 review with `tests/it/transfer.rs`.

- [ ] `src/tracing/arena.rs`
  - Pending round-2 review.

- [ ] `src/tracing/builder/geth.rs`
  - Pending round-2 review with `tests/it/geth.rs`.

- [ ] `src/tracing/builder/mod.rs`
  - Pending round-2 review.

- [ ] `src/tracing/builder/parity.rs`
  - Pending round-2 review with `tests/it/parity.rs`.

- [ ] `src/tracing/builder/walker.rs`
  - Pending round-2 review.

- [ ] `src/tracing/config.rs`
  - Pending round-2 review.

- [ ] `src/tracing/debug.rs`
  - Pending round-2 review with `tests/it/geth.rs` and `tests/it/geth_js.rs`.

- [ ] `src/tracing/fourbyte.rs`
  - Pending round-2 review.

- [ ] `src/tracing/js/bindings.rs`
  - Pending round-2 review with JS tracer tests.

- [ ] `src/tracing/js/builtins.rs`
  - Pending round-2 review with `tests/it/test_native_bigint.rs`.

- [ ] `src/tracing/js/mod.rs`
  - Pending round-2 review with `tests/it/geth_js.rs`.

- [ ] `src/tracing/mod.rs`
  - Pending round-2 review with tracing integration tests.

- [ ] `src/tracing/mux.rs`
  - Pending round-2 review with mux coverage in `tests/it/geth.rs`.

- [ ] `src/tracing/opcount.rs`
  - Pending round-2 review.

- [ ] `src/tracing/types.rs`
  - Pending round-2 review with geth/parity/writer tests.

- [ ] `src/tracing/utils.rs`
  - Pending round-2 review.

- [ ] `src/tracing/writer.rs`
  - Pending round-2 review with `tests/it/writer.rs`.

## Integration Test Files

- [ ] `tests/it/accesslist.rs`
  - Pending round-2 review with `src/access_list.rs`.

- [ ] `tests/it/edge_cov.rs`
  - Intentionally omitted with `src/edge_cov.rs`.

- [ ] `tests/it/geth.rs`
  - Pending round-2 review with geth tracing/debug/prestate files.

- [ ] `tests/it/geth_js.rs`
  - Pending round-2 review with JS tracer files.

- [ ] `tests/it/main.rs`
  - Pending round-2 review.

- [ ] `tests/it/parity.rs`
  - Pending round-2 review with `src/tracing/builder/parity.rs`.

- [ ] `tests/it/repro/mod.rs`
  - Pending round-2 review.

- [ ] `tests/it/repro/prestate.rs`
  - Pending round-2 review with prestate builders.

- [ ] `tests/it/test_native_bigint.rs`
  - Pending round-2 review with JS builtins.

- [ ] `tests/it/transfer.rs`
  - Pending round-2 review with `src/transfer.rs`.

- [ ] `tests/it/utils.rs`
  - Pending round-2 review. This is expected to be an evm2 harness, but random production-facing
    structs or traits are not expected.

- [ ] `tests/it/writer.rs`
  - Pending round-2 review with writer snapshots.

## Fixture Coverage

- [ ] `testdata/Counter.sol`
  - Pending byte-for-byte comparison.

- [ ] `testdata/repro/tx-selfdestruct.json`
  - Pending byte-for-byte comparison.

- [ ] `tests/it/writer/**`
  - Pending snapshot file comparison.
