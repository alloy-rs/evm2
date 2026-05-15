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

- [x] `src/lib.rs`
  - API surface matches except `edge_cov` is intentionally not exported.
  - Dependency keepalive imports are port-only crate hygiene.
  - Removed the port-only root `OpcodeGasInspector`/`immediate_size` re-export to match upstream.

- [x] `src/access_list.rs`
  - Public type and methods match; `excluded()`/`touched_slots()` are additionally `const fn`.
  - Top-level exclusion semantics match upstream shape: caller, call/create target, precompiles, and recovered EIP-7702 authorities.
  - Opcode handling is equivalent after evm2 stack/message substitutions.

- [x] `src/edge_cov.rs`
  - Intentionally omitted from the evm2 port.

- [x] `src/opcode.rs`
  - Public type and methods match except `immediate_size` takes an opcode byte because evm2 does not implement RJUMPV bytecode immediate decoding.
  - Opcode counting and call/create gas-limit subtraction match upstream, with root skip mapped to evm2 message depth `0`.
  - Embedded tests are ported to the evm2 interpreter harness.

- [x] `src/storage.rs`
  - Public API and SLOAD counting semantics match.
  - Address source maps from upstream target address to evm2 `message().destination`.

- [x] `src/transfer.rs`
  - Public API matches upstream after removing port-only `logs()`.
  - `with_logs()` emits ERC20-style transfer logs through the evm2 host instead of directly journaling through revm.
  - Call, CALLCODE, create, create2, zero-value skip, internal-only depth skip, and selfdestruct recording match upstream semantics.

- [x] `src/tracing/arena.rs`
  - Semantically identical; `nodes_mut()` is additionally `const fn`.

- [ ] `src/tracing/builder/geth.rs`
  - Pending round-2 review with `tests/it/geth.rs`.

- [x] `src/tracing/builder/mod.rs`
  - Identical.

- [ ] `src/tracing/builder/parity.rs`
  - Pending round-2 review with `tests/it/parity.rs`.

- [x] `src/tracing/builder/walker.rs`
  - Identical.

- [x] `src/tracing/config.rs`
  - Public API and configuration semantics match.
  - `OpcodeFilter::is_enabled` and `enable` are additionally `const fn`.

- [ ] `src/tracing/debug.rs`
  - Pending round-2 review with `tests/it/geth.rs` and `tests/it/geth_js.rs`.

- [x] `src/tracing/fourbyte.rs`
  - Public API and selector/count output match.
  - Upstream handles shared-memory call input; evm2 messages already carry materialized input bytes.

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

- [x] `src/tracing/opcount.rs`
  - Public API and behavior match.

- [x] `src/tracing/types.rs`
  - Public trace data types and methods match after evm2 type substitutions.
  - `InstructionResult` is replaced by evm2 `InstrStop`; `InstrStop` now derives serde under the `serde` feature so status fields remain serialized like upstream.
  - `From<CallScheme>` and `From<CreateScheme>` cannot exist without revm input types; equivalent `MessageKind` mapping lives in `tracing/mod.rs`.

- [x] `src/tracing/utils.rs`
  - Revert decoding, memory conversion, and gas-used behavior match.
  - Error mapping is ported from `InstructionResult` to evm2 `InstrStop`; `InvalidFEOpcode` is folded into invalid opcode because evm2 does not expose it separately.
  - `load_account_code` uses `&mut dyn DynDatabase` and returns `DbResult<Option<Bytes>>`; callers preserve the original optional-code behavior.

- [x] `src/tracing/writer.rs`
  - Writer public API and formatting behavior match after `InstrStop` substitution.
  - Several builder/getter methods are additionally `const fn`.

## Integration Test Files

- [x] `tests/it/accesslist.rs`
  - Original test is present with only evm2 helper/API substitutions and bytecode formatting.

- [x] `tests/it/edge_cov.rs`
  - Intentionally omitted with `src/edge_cov.rs`.

- [ ] `tests/it/geth.rs`
  - Pending round-2 review with geth tracing/debug/prestate files.

- [ ] `tests/it/geth_js.rs`
  - Pending round-2 review with JS tracer files.

- [x] `tests/it/main.rs`
  - Same module list except intentional `edge_cov` omission.
  - `geth_js` is gated by `std` plus `js-tracer` because the evm2 test harness requires `std`.

- [ ] `tests/it/parity.rs`
  - Pending round-2 review with `src/tracing/builder/parity.rs`.

- [ ] `tests/it/repro/mod.rs`
  - Pending round-2 review.

- [ ] `tests/it/repro/prestate.rs`
  - Pending round-2 review with prestate builders.

- [x] `tests/it/test_native_bigint.rs`
  - Identical aside from import ordering.

- [x] `tests/it/transfer.rs`
  - Original test is present with only evm2 helper/API substitutions and bytecode formatting.
  - Adds `records_failed_create_transfer_attempt`, which confirms upstream-equivalent pre-execution create transfer recording for evm2.

- [ ] `tests/it/utils.rs`
  - Pending round-2 review. This is expected to be an evm2 harness, but random production-facing
    structs or traits are not expected.

- [x] `tests/it/writer.rs`
  - Original tests and patching helper are present with only evm2 helper/API substitutions and import ordering changes.

## Fixture Coverage

- [ ] `testdata/Counter.sol`
  - Pending byte-for-byte comparison.

- [ ] `testdata/repro/tx-selfdestruct.json`
  - Pending byte-for-byte comparison.

- [ ] `tests/it/writer/**`
  - Pending snapshot file comparison.
