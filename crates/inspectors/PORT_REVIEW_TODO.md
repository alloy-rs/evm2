# revm-inspectors Port Review Todo

Baseline: `/home/doni/github/paradigmxyz/revm-inspectors` at `6566b83`.

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

- [x] `src/tracing/builder/geth.rs`
  - Geth call/default/flat traces match after evm2 type substitutions.
  - Prestate diff mode matches the upstream state-diff-driven builder.
  - Prestate default mode intentionally records DB-backed caller/callee and opcode-touched accounts,
    and reconstructs read storage from recorded storage steps, because evm2 `StateChanges` does not
    carry revm's full touched-account/read-slot journal shape.

- [x] `src/tracing/builder/mod.rs`
  - Identical.

- [x] `src/tracing/builder/parity.rs`
  - Parity trace, localized trace, transaction trace iterator, VM trace, and selfdestruct ordering
    match upstream after evm2 type substitutions.
  - `into_trace_results` takes output bytes directly because evm2 result extraction happens in the
    caller/helper layer.
  - State diff and VM bytecode population use evm2 `StateChanges`/`DynDatabase`; this is the
    expected replacement for revm's `ResultAndState`/`DatabaseRef`/account flags.

- [x] `src/tracing/builder/walker.rs`
  - Identical.

- [x] `src/tracing/config.rs`
  - Public API and configuration semantics match.
  - `OpcodeFilter::is_enabled` and `enable` are additionally `const fn`.
  - `from_geth_prestate_config` enables steps/state diffs/stack snapshots so the evm2 prestate
    builder can recover touched accounts and read storage that upstream gets from revm state.

- [x] `src/tracing/debug.rs`
  - Built-in tracer dispatch and `fuse` behavior match after evm2 DB/error type substitutions.
  - `Noop` is a unit variant because evm2 has no `NoOpInspector` adapter type.
  - Missing `log_full`/frame hooks are expected port-wide differences.

- [x] `src/tracing/fourbyte.rs`
  - Public API and selector/count output match.
  - Upstream handles shared-memory call input; evm2 messages already carry materialized input bytes.

- [x] `src/tracing/js/bindings.rs`
  - JS-facing objects and method/property names match upstream after evm2 type substitutions.
  - Stack and memory wrappers preserve upstream lifetime-guard semantics with evm2 stack/memory
    references.
  - DB access is split into live-state and post-result readers because evm2 exposes `State` during
    execution and `StateChanges` after execution instead of revm `EvmState + DatabaseRef`.

- [x] `src/tracing/js/builtins.rs`
  - Builtin functions and tests match upstream; differences are import ordering and Boa API
    borrowing adjustments only.

- [x] `src/tracing/js/mod.rs`
  - Public `JsInspector` API matches upstream after evm2 transaction/result/database substitutions.
  - Removed port-only public `code()` accessor to keep the upstream API surface.
  - Step/fault/enter/exit/selfdestruct callback behavior matches upstream; DB objects use evm2 live
    state during execution and post-transaction changes for `result`.

- [x] `src/tracing/mod.rs`
  - `TracingInspector` public API and call/create/selfdestruct/step/log recording semantics match
    after evm2 type substitutions.
  - Deprecated upstream getters/builders remain intentionally omitted.
  - Push-stack snapshots now use opcode output arity like upstream, including zero-output steps.
  - Log `position` now follows upstream child-index positioning while `index` remains global.
  - Missing `log_full`/frame hooks are expected port-wide differences.

- [x] `src/tracing/mux.rs`
  - Tracer configuration, mux delegation, and `get_result` behavior match after evm2
    `StateChanges`/`DynDatabase` substitutions.
  - Missing `log_full`/frame hooks are expected port-wide differences.

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

- [x] `tests/it/geth.rs`
  - Original geth tracing tests are present with evm2 helper/API substitutions and bytecode
    formatting only.

- [x] `tests/it/geth_js.rs`
  - Original geth JS tracer tests are present with evm2 helper/API substitutions and bytecode
    formatting.
  - Adds coverage for the evm2 `DebugInspector` JS tracer result path and DB reader adapter.

- [x] `tests/it/main.rs`
  - Same module list except intentional `edge_cov` omission.
  - `geth_js` is gated by `std` plus `js-tracer` because the evm2 test harness requires `std`.

- [x] `tests/it/parity.rs`
  - Original parity tests are present with evm2 helper/API substitutions and bytecode formatting.
  - Typo-only correction: `contect created` to `context created`.

- [x] `tests/it/repro/mod.rs`
  - Original repro helper structure is present with evm2 cache/account/storage substitutions.
  - Hardfork lookup uses `alloy_hardforks::EthereumHardfork::from_mainnet_block_number`; forks
    without EVM semantic changes are folded to the active evm2 `SpecId`, and unknown future forks
    map to `SpecId::NEXT`.

- [x] `tests/it/repro/prestate.rs`
  - Original prestate repro tests are present with evm2 helper/API substitutions.
  - Builder calls pass evm2 `StateChanges` plus a mutable DB clone instead of revm `ResultAndState`
    plus `DatabaseRef`.

- [x] `tests/it/test_native_bigint.rs`
  - Identical aside from import ordering.

- [x] `tests/it/transfer.rs`
  - Original test is present with only evm2 helper/API substitutions and bytecode formatting.
  - Adds `records_failed_create_transfer_attempt`, which confirms upstream-equivalent pre-execution create transfer recording for evm2.

- [x] `tests/it/utils.rs`
  - This is an evm2-only test harness replacing revm's `Context`/`InspectEvm` conveniences.
  - The additional structs/traits are test-only shims for transaction envs, result wrappers,
    inspector slots, deployment helpers, and DB commit/load helpers; no production API surface is
    added here.

- [x] `tests/it/writer.rs`
  - Original tests and patching helper are present with only evm2 helper/API substitutions and import ordering changes.

## Fixture Coverage

- [x] `testdata/Counter.sol`
  - Byte-for-byte identical to upstream.

- [x] `testdata/repro/tx-selfdestruct.json`
  - Byte-for-byte identical to upstream.

- [x] `tests/it/writer/**`
  - File list and contents are byte-for-byte identical to upstream.
