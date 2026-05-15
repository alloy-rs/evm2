# revm-inspectors Port Review Todo

Baseline: `/home/doni/github/paradigmxyz/revm-inspectors`.

Port: `crates/inspectors`.

Scope: every original Rust source file under `src/` and every original Rust integration test file
under `tests/it/`. Testdata and writer snapshot files were also compared byte-for-byte.

Expected port-wide differences:
- `revm` context/interpreter/database types are replaced by `evm2` types.
- `Inspector::log_full`, `frame_start`, and `frame_end` hooks are absent in the `evm2` inspector API.
- Imports, formatting, docs, and test harness setup differ.

## Source Files

- [x] `src/lib.rs`
  - Same module set is present.
  - Root docs were shortened and changed from revm to evm2.
  - Adds dependency keepalive imports.
  - Adds root `pub use opcode::{OpcodeGasInspector, immediate_size};`; original did not root-reexport these.

- [x] `src/access_list.rs`
  - Public type and methods are present.
  - `excluded()` and `touched_slots()` are now `const fn`.
  - Top-level exclusion collection is not semantically identical: original excluded caller, call target or computed CREATE address, precompiles, and EIP-7702 authorization authorities. Port excludes `message.caller`, `message.destination`, and precompile warm addresses only.
  - SLOAD/SSTORE and account-touch opcode handling is otherwise structurally equivalent using evm2 stack/message access.

- [x] `src/edge_cov.rs`
  - Intentionally omitted from the evm2 port.
  - The public `edge_cov` module and `EdgeCovInspector` are not exported.

- [x] `src/opcode.rs`
  - Public type and methods are present.
  - `immediate_size` API changed from `pub fn immediate_size(bytecode: &impl Immediates) -> u8` to `pub fn immediate_size(opcode: u8) -> u8`; original bytecode/RJUMPV special-case shape is not preserved.
  - Call/create gas-limit subtraction is ported, but root-depth check changed from revm journal depth `0` to evm2 message depth `1`.
  - Embedded tests are present with evm2 harness rewrites.

- [x] `src/storage.rs`
  - Public type and methods are present.
  - `accessed_slots()` was already `const`; behavior is structurally equivalent for SLOAD counting.
  - Address source changed to `message().destination`.

- [x] `src/transfer.rs`
  - Public type and core methods are present.
  - Adds public `logs()` method and `logs: Vec<Log>` storage; this is a new API surface not present upstream.
  - `new`, `internal_only`, and `with_logs` are now `const fn`.
  - Log insertion changed from journaling a log into revm state to emitting via evm2 `Host::log` and retaining a copy in the inspector.
  - `TransferOperation` and `TransferKind` gained `Copy`.
  - Create target comes from `message.destination` instead of computing from caller nonce.

- [x] `src/tracing/arena.rs`
  - Semantically identical except `nodes_mut()` is now `const fn`.

- [x] `src/tracing/builder/geth.rs`
  - Public builder remains, but constructors now require `spec_id: Option<SpecId>`.
  - Prestate APIs changed from `ResultAndState + DatabaseRef` to `StateChanges + &mut dyn DynDatabase` and now return `Infallible` errors.
  - Prestate pre-mode now seeds caller/address entries from trace nodes; original iterated revm state accounts.
  - Diff-mode logic is materially rewritten for evm2 `StateChanges`, including special post-code fallback through `state.code` and spec-based selfdestruct cleanup.
  - ERC-7562 and code-size database reads are ported through dynamic DB helpers.
  - Embedded `prestate_diff_keeps_prefunded_created_accounts` is present with evm2 state changes.

- [x] `src/tracing/builder/mod.rs`
  - Identical.

- [x] `src/tracing/builder/parity.rs`
  - Public builder remains, but result/state APIs now take output bytes and evm2 `StateChanges`.
  - Adds `into_trace_results_with_state_and_db` and `populate_state_diff_with_db`.
  - `populate_state_diff` no longer accepts a DB; no-DB and with-DB behavior differ, and the no-DB path does not mirror the original DB-backed creation/selfdestruct filtering exactly.
  - `populate_vm_trace_bytecodes` now silently leaves code empty without a DB instead of using `DatabaseRef`.
  - Embedded selfdestruct tests are present.

- [x] `src/tracing/builder/walker.rs`
  - Identical.

- [x] `src/tracing/config.rs`
  - Semantically identical.
  - `OpcodeFilter::is_enabled` and `enable` are now `const fn`.
  - Embedded config tests are present.

- [x] `src/tracing/debug.rs`
  - Built-in tracer construction is preserved.
  - `Noop` variant changed from `NoOpInspector` payload to a unit variant.
  - `get_result` signature changed to evm2 `RecoveredTxEnvelope`, `BlockEnv`, `TxResult`, and `DynDatabase`.
  - `DebugInspectorError` lost the generic DB error parameter and `Database` variant.
  - Missing `log_full` and frame hooks are expected.

- [x] `src/tracing/fourbyte.rs`
  - Public type and `inner()` are present.
  - Call input extraction is simplified to evm2 `message.input`; semantics are equivalent if evm2 always materializes call input bytes.

- [x] `src/tracing/js/bindings.rs`
  - JS exposed objects and methods are largely preserved.
  - Private DB plumbing was substantially rewritten: original `StateRef`, `GcDb`, `JsDb`, and `StringError` were replaced by `EvmDbReader`, `StateDbReader`, `ChangesDbReader`, and `EvmDbGuard`.
  - Adds private `StackRefInner` and owned stack/memory test constructors.
  - Adds embedded `test_evm_db_reads_backing_dyn_database`; all original embedded tests are present.

- [x] `src/tracing/js/builtins.rs`
  - Semantically identical; only formatting/import/test-harness adjustments.
  - Embedded builtin tests are present.

- [x] `src/tracing/js/mod.rs`
  - Public JS inspector exists, with `config`, `transaction_context`, runtime limits, `try_clone`, `json_result`, and `result`.
  - Adds public `code()` method; original did not expose it.
  - `try_clone` now preserves transaction context; original cloned with default transaction context.
  - `json_result`/`result` signatures changed to evm2 transaction/result/database types.
  - JS hook error-to-revert output, precompile registration timing, and create-exit error reporting now match the original shape.
  - Embedded tests are present with evm2 harness rewrites.

- [x] `src/tracing/mod.rs`
  - Public `TracingInspector` and transaction context APIs are mostly present.
  - Deprecated original public methods `get_traces`, `get_traces_mut`, and `with_transaction_gas_limit` are intentionally omitted.
  - Adds private `PendingStorageStep` to model revm journal storage-diff recording through evm2 state hooks.
  - `fuse()` no longer clears `spec_id`; original cleared it.
  - Step recording, storage diff recording, and call lifecycle were materially rewritten around evm2 hooks/state.
  - `CallInputExt` helper trait was removed.

- [x] `src/tracing/mux.rs`
  - Mux configuration behavior is preserved.
  - `try_into_mux_frame` signature changed to gas/state/tx_info/optional dynamic DB and `Infallible` errors.
  - Missing `log_full` and frame hooks are expected.

- [x] `src/tracing/opcount.rs`
  - Semantically identical.

- [x] `src/tracing/types.rs`
  - Public trace data types are present.
  - `InstructionResult` replaced by `InstrStop`.
  - `CallTrace.status` and `CallTraceStep.status` are now skipped under serde; original serialized status when revm serde support allowed it.
  - Removed `From<CallScheme>` and `From<CreateScheme>` impls; replacement `From<MessageKind>` lives in `tracing/mod.rs`.
  - Other changes are formatting/const/type substitutions.

- [x] `src/tracing/utils.rs`
  - Revert decoding tests and helper behavior are present.
  - `fmt_error_msg` maps evm2 `InstrStop`; original `InvalidFEOpcode` branch is gone and merged through invalid opcode handling.
  - `load_account_code` now needs `&mut dyn DynDatabase`; DB errors are swallowed as `None` like the original optional helper behavior.

- [x] `src/tracing/writer.rs`
  - Writer output logic is equivalent aside from `InstrStop` substitution.
  - Several getters/builders became `const fn`.

## Integration Test Files

- [x] `tests/it/accesslist.rs`
  - Original test is present.
  - Differences are imports, helper types, bytecode formatting, and DB insertion API.

- [x] `tests/it/edge_cov.rs`
  - Intentionally omitted with `src/edge_cov.rs`.

- [x] `tests/it/geth.rs`
  - All original test functions are present.
  - Differences are imports, evm2 helper APIs, `DebugInspector::get_result`/mux/prestate call shapes, opcode namespace changes, and bytecode formatting.
  - Prestate tests now pass cloned DBs to preserve original code/prestate population semantics.

- [x] `tests/it/geth_js.rs`
  - Original tests are present.
  - Adds `test_geth_debug_inspector_jstracer`.
  - Adds `js_result` helper for evm2 result/context conversion.

- [x] `tests/it/main.rs`
  - Same modules are present.
  - `geth_js` is now gated by both `std` and `js-tracer`, not only `js-tracer`.

- [x] `tests/it/parity.rs`
  - All original test functions are present.
  - Differences are helper/API rewrites and blob-fee env adaptation.
  - State-diff population now uses the DB-backed `populate_state_diff(state_diff, &res.state, db)` shape.

- [x] `tests/it/repro/mod.rs`
  - File is ported, but hardfork mapping changed materially: dependency on `alloy_hardforks` was removed, and DAO/Muir Glacier/Arrow Glacier/Gray Glacier block ranges no longer map to their original SpecIds.
  - DB construction otherwise follows the original fixture prestate intent.

- [x] `tests/it/repro/prestate.rs`
  - Original tests are present.
  - Prestate builder calls now pass evm2 state changes and `None` DB.

- [x] `tests/it/test_native_bigint.rs`
  - Semantically identical; import order only.

- [x] `tests/it/transfer.rs`
  - Original test is present.
  - Adds `records_failed_create_transfer_attempt`.
  - Other differences are helper/API rewrites.

- [x] `tests/it/utils.rs`
  - This is intentionally not a direct port: it replaces the revm test harness with a large evm2 harness.
  - Adds local `TxEnv`, `Context`, `ResultAndState`, `ExecutionResult`, `Output`, `DeployResult`, `DatabaseCommit`, `TestDbExt`, and `InspectorSlot`.
  - Adds `TracingInspectorExt::with_transaction_gas_limit` only for tests, which masks the production `TracingInspector` API missing that original method.
  - `TestEvmWithInspector::inspect_tx` now relies on inspector-recorded storage diffs directly.

- [x] `tests/it/writer.rs`
  - Original tests are present.
  - Differences are imports and helper API rewrites only.

## Test And Fixture Coverage

- [x] Original Rust test function names: all original names are present.
  - Added tests in port: `records_failed_create_transfer_attempt`, `test_evm_db_reads_backing_dyn_database`, `test_geth_debug_inspector_jstracer`.

- [x] Non-Rust testdata and writer snapshots:
  - `testdata/Counter.sol` matches byte-for-byte.
  - `testdata/repro/tx-selfdestruct.json` matches byte-for-byte.
  - `tests/it/writer/**` expected snapshot files match byte-for-byte.

## Open Review Items

- [x] Decide whether production API compatibility should include original `TracingInspector::with_transaction_gas_limit`, `get_traces`, and `get_traces_mut`.
- [x] Decide whether `OpcodeGasInspector::immediate_size` must preserve the original bytecode-based signature/RJUMPV behavior.
- [x] Revisit access-list exclusion semantics for CREATE and EIP-7702 authorities.
- [x] Revisit JS tracer thrown-error revert output, precompile registration timing, and create-exit error reporting.
- [x] Revisit prestate/state-diff behavior that relies on `fill_storage_changes` post-processing.
- [x] Revisit historical hardfork mapping in `tests/it/repro/mod.rs`.
