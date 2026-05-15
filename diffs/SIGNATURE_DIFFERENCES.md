# Signature Differences

Baseline: `/home/doni/github/paradigmxyz/revm-inspectors`.

Port: `/home/doni/github/danipopes/evm2.1/crates/inspectors`.

This file tracks public or behaviorally relevant function/method signature differences from upstream. Import-only differences are intentionally ignored.

## Reviewed So Far

- `src/access_list.rs`
  - `impl Inspector` methods use evm2 `Message<T>`/`MessageResult<T>`/host parameters instead of revm `ContextTr`/`CallInputs`/`CreateInputs`.
  - `collect_excluded_addresses` takes an evm2 top-level `Message` and `Evm` host instead of a revm context.

- `src/opcode.rs`
  - `impl Inspector` methods use evm2 `Message<T>`/`MessageResult<T>`/host parameters instead of revm context/input/outcome types.
  - `immediate_size` takes an opcode byte (`u8`) instead of `&impl Immediates`, because evm2 bytecode does not expose upstream RJUMPV immediate decoding.

- `src/storage.rs`
  - `impl Inspector::step` uses evm2 `Interpreter<'_, T>`/host parameters instead of revm `Interpreter`/context.

- `src/tracing/builder/geth.rs`
  - `GethTraceBuilder::new` and `new_borrowed` take `spec_id: Option<SpecId>` so diff prestate can account for Cancun selfdestruct behavior without revm context.
  - `geth_prestate_traces` takes `&StateChanges` and `&mut dyn DynDatabase` instead of `&ResultAndState` and `DatabaseRef`.
  - Internal prestate helpers take `StateChanges`/`DynDatabase` and return `DbResult`.
  - `geth_erc7562_traces` takes `&mut dyn DynDatabase` and returns `DbResult<Erc7562Frame>` so code-size DB errors are propagated.

- `src/tracing/builder/parity.rs`
  - `into_trace_results` takes output bytes directly instead of an upstream `ExecutionResult`.
  - `into_trace_results_with_state` takes output bytes, `&StateChanges`, and `&mut dyn DynDatabase` instead of `&ResultAndState` plus `DatabaseRef`.
  - `populate_vm_trace_bytecodes` and `populate_state_diff` use `&mut dyn DynDatabase` and `&StateChanges`.

- `src/tracing/debug.rs`
  - `DebugInspector::Noop` is a unit variant instead of storing revm's `NoOpInspector`.
  - `DebugInspector::get_result` takes evm2 `RecoveredTxEnvelope`, `BlockEnv`, `TxResult<T>`, and `&mut dyn DynDatabase`.
  - `impl Inspector` hooks use evm2 generic `EvmTypes`, `Interpreter<'_, T>`, `Message<T>`, `MessageResult<T>`, `Log`, and host parameters.
  - `DebugInspectorError` stores evm2 `DbErrorCode` instead of being generic over `DB::Error`.

- `src/tracing/js/mod.rs`
  - `JsInspector::json_result` and `result` take evm2 `TxResult<T>`, `RecoveredTxEnvelope`, `BlockEnv`, and `&mut dyn DynDatabase`.
  - `impl Inspector` hooks use evm2 message/result/interpreter/host types.
  - Internal `push_call` takes an evm2 `Message<T>` instead of split call fields.

- `src/tracing/js/bindings.rs`
  - `MemoryRef::new` takes evm2 `Memory`; `StackRef::new` takes evm2 `StackRef<'_>`.
  - `EvmDbRef::new_state` and `new_changes` replace upstream `EvmDbRef::new(EvmState, DatabaseRef)` with evm2 `State`/`StateChanges` readers.

- `src/tracing/mod.rs`
  - `TracingInspector` implements evm2 `Inspector<T>` hooks instead of revm `Inspector<CTX>`.
  - `geth_builder` and `into_geth_builder` pass `spec_id` into `GethTraceBuilder`.
  - Deprecated `get_traces`/`get_traces_mut` are intentionally absent.

- `src/tracing/mux.rs`
  - `MuxInspector::try_into_mux_frame` takes `gas_used`, `&StateChanges`, `TransactionInfo`, and `&mut dyn DynDatabase` instead of `&ResultAndState` and `DatabaseRef`.
  - `impl Inspector` hooks use evm2 message/result/interpreter/host types.

- `src/tracing/types.rs`
  - Trace statuses use evm2 `InstrStop` instead of revm `InstructionResult`.
  - revm `CallScheme`/`CreateScheme` conversion impls are absent; evm2 `MessageKind` conversion lives in `tracing/mod.rs`.

- `src/tracing/utils.rs`
  - `fmt_error_msg` takes evm2 `InstrStop`.
  - `load_account_code` takes `&mut dyn DynDatabase` and returns `DbResult<Option<Bytes>>` instead of accepting a generic `DatabaseRef` and swallowing DB errors.

- `src/transfer.rs`
  - `impl Inspector` hooks use evm2 `Message<T>`/`MessageResult<T>` and host logging instead of revm context/journal inputs.
  - Internal `on_transfer` takes call depth and an emit-log closure instead of a revm journal.

Clippy-only `const fn` upgrades are intentionally not listed as actionable signature drift.
