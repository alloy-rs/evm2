# Inspectors Source Audit

Upstream reference: `/home/doni/github/paradigmxyz/revm-inspectors` at `b663800`.

Scope: compare every Rust source file under `crates/inspectors/src` against the corresponding upstream source file, one by one. Differences below separate expected evm2 porting changes from concrete missing or behavior-relevant upstream pieces.

File-set check: upstream and local both contain the same 23 Rust source files under `src`; no Rust source file is missing or extra.

## Progress

- [x] `src/access_list.rs`
- [x] `src/edge_cov.rs`
- [x] `src/lib.rs`
- [x] `src/opcode.rs`
- [x] `src/storage.rs`
- [x] `src/tracing/arena.rs`
- [x] `src/tracing/builder/geth.rs`
- [x] `src/tracing/builder/mod.rs`
- [x] `src/tracing/builder/parity.rs`
- [x] `src/tracing/builder/walker.rs`
- [x] `src/tracing/config.rs`
- [x] `src/tracing/debug.rs`
- [x] `src/tracing/fourbyte.rs`
- [x] `src/tracing/js/bindings.rs`
- [x] `src/tracing/js/builtins.rs`
- [x] `src/tracing/js/mod.rs`
- [x] `src/tracing/mod.rs`
- [x] `src/tracing/mux.rs`
- [x] `src/tracing/opcount.rs`
- [x] `src/tracing/types.rs`
- [x] `src/tracing/utils.rs`
- [x] `src/tracing/writer.rs`
- [x] `src/transfer.rs`

## Findings

### `src/access_list.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `ContextTr`, `JournalExt`, `CallInputs`, and `CreateInputs` to evm2 `Inspector<T>`, `Interpreter<'_, T>`, and `Message<T>` hooks with the new `host` parameter ignored.
- Opcode access changed from `interp.bytecode.opcode()` and `interp.stack.peek(...) -> Result` to evm2 `interp.opcode()` and `interp.stack().peekn()/peek(...) -> Option`.
- Current contract for `SLOAD`/`SSTORE` changed from revm `interp.input.target_address()` to evm2 `interp.message().destination`.
- Excluded-address collection is not equivalent to upstream:
  - Upstream excludes the tx caller, tx target or derived create address, the active precompile address set from the journal, and EIP-7702 authority addresses.
  - Local evm2 port excludes `message.caller`, `message.destination`, hard-coded addresses `0x01..=0x11`, and hard-coded `0x100`.
  - This means EIP-7702 authorities are not excluded, the create address is not derived from caller nonce, and precompile exclusions are hard-coded rather than sourced from the configured host/precompile provider.
- Access-list public helpers are otherwise present: `new`, `excluded`, `touched_slots`, `into_touched_slots`, `into_access_list`, and `access_list`.

Assessment: behavior gap remains in excluded-address calculation because evm2 does not currently pass full transaction/precompile/auth context into this inspector path.

### `src/edge_cov.rs`

Status: reviewed.

Concrete differences:

- Ported imports and inspector impl from revm to evm2.
- Address used for edge hashing changed from revm `interp.input.target_address()` to evm2 `interp.message().code_address`, which is the better equivalent for delegated-code execution.
- Stack access changed from revm `Result`-returning `peek` to evm2 `Option`-returning `peek`.
- The `JUMPI` branch condition is equivalent after porting: nonzero condition records target stack item, zero condition records fallthrough `pc + 1`.
- Documentation/comments were shortened; no API item or behavior from upstream is missing.

Assessment: no missing upstream behavior found.

### `src/tracing/builder/geth.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `ResultAndState`, `EvmState`, `DatabaseRef`, `HaltReasonTr`, and revm opcode constants to evm2 `StateChanges`, `SpecId`, and evm2 opcode constants.
- Builder constructors now carry `spec_id: Option<SpecId>` so local code can handle pre-Cancun selfdestruct behavior without revm context.
- `geth_traces` and `geth_call_traces` are otherwise structurally preserved.
- Prestate tracing is materially different:
  - Upstream receives `ResultAndState` plus a `DatabaseRef`, reads pre-transaction account/code/storage from the database, and returns `Result<PreStateFrame, DB::Error>`.
  - Local evm2 port receives only `StateChanges`, uses `Tracked::original/current`, and returns `Result<_, Infallible>`.
  - Local prestate mode also seeds default entries for caller/address pairs seen in recorded trace nodes.
  - Because there is no database reference, local code cannot fetch code by hash if it is not present in `StateChanges`.
- Diff-mode cleanup via `diff_traces` is present locally after the follow-up fix.
- Local diff mode has extra evm2-specific handling:
  - uses `StateChanges::code` as a fallback when post code is missing but `code_enabled` is set;
  - removes selfdestructed accounts from post state for specs before Cancun using the stored `spec_id`.
- ERC-7562 tracing is incomplete relative to upstream:
  - Upstream accepts `db: DatabaseRef` and fills `contract_size` for `EXTCODESIZE`, `EXTCODECOPY`, and `EXTCODEHASH`.
  - Local evm2 port removed the DB parameter and currently leaves `contract_size` unfilled (`Entry::Vacant` is ignored).
- Test `prestate_diff_keeps_prefunded_created_accounts` was ported from revm `EvmState + CacheDB` to evm2 `StateChanges`.

Assessment: real behavior gaps remain where upstream depends on read-only database access, especially prestate code lookup and ERC-7562 contract size enrichment.

### `src/tracing/builder/parity.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `ExecutionResult`, `ResultAndState`, `DatabaseRef`, `Account`, and `load_account_code` to evm2 `StateChanges` and explicit output `Bytes`.
- Trace tree construction, selfdestruct ordering, VM trace shape, and transaction trace generation are otherwise preserved.
- `into_trace_results` now takes `output: Bytes` directly instead of deriving it from revm `ExecutionResult`.
- `into_trace_results_with_state` now takes `output: Bytes` and `&StateChanges`, returns `Result<_, Infallible>`, and no longer accepts a DB.
- `populate_vm_trace_bytecodes` is not equivalent:
  - Upstream walks breadth-first addresses and fills each `VmTrace.code` from `DatabaseRef` account code or code hash.
  - Local evm2 port consumes the addresses only to keep traversal shape and does not fill bytecode.
- `populate_state_diff` is not equivalent:
  - Upstream compares changed revm accounts against DB pre-state, handles created/selfdestructed-created accounts specially, loads code through `load_account_code`, and filters unchanged accounts.
  - Local evm2 port derives deltas directly from `StateChanges::accounts` and `StateChanges::storage`, using only code already embedded in `AccountInfo`.

Assessment: real behavior gaps remain for Parity `VmTrace.code` and DB-backed state diff fidelity.

### `src/tracing/config.rs`

Status: reviewed.

Concrete differences:

- Only substantive source change is importing `OpCode` from evm2 instead of revm.
- Public configuration types and constructors match upstream.

Assessment: no missing upstream behavior found.

### `src/tracing/debug.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `ContextTr`, `Transaction`, `Block`, `ResultAndState`, `DatabaseRef`, `FrameInput`, and `FrameResult` to evm2-specific helper traits:
  - `TraceTxEnv`
  - `TraceBlockEnv`
  - `DebugTraceResult`
- Local `DebugInspector::Noop` is a plain enum variant instead of wrapping revm `NoOpInspector`.
- JS tracer support is not equivalent:
  - Upstream has `DebugInspector::Js(Box<JsInspector>)` behind `js-tracer` and constructs it from `GethDebugTracerType::JsTracer`.
  - Local `DebugInspector::new` always returns `JsTracerNotEnabled` for JS tracers, even when the crate has `js-tracer` support elsewhere.
- Result finalization is not equivalent:
  - Upstream `get_result` receives tx env, block env, `ResultAndState`, and mutable DB, then passes DB into prestate, mux, ERC-7562, and JS paths.
  - Local `get_result` receives `DebugTraceResult` and no DB; it depends on the reduced evm2 builders described above.
- Local `TraceTxEnv for TxEnv<T>` returns `0` for `trace_gas_limit()` because evm2's generic `TxEnv` does not store the transaction gas limit. This means `set_transaction_gas_limit` gets `0` unless callers provide their own `TraceTxEnv` implementation.
- Delegation is manually expanded instead of upstream's `delegate!` macro.
- Upstream delegates `log_full`, `frame_start`, and `frame_end`; local evm2 inspector trait has no corresponding hooks, so these are absent.
- `DebugInspectorError` no longer carries DB errors or JS inspector errors because those paths were removed from this wrapper.

Assessment: real behavior gaps remain for JS tracer wiring, frame hook delegation, database-backed trace finalization, and transaction gas-limit propagation through the default `TxEnv` implementation.

### `src/tracing/fourbyte.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `CallInputs` and shared-memory input handling to evm2 `Message<T>`.
- Upstream handles both `CallInput::SharedBuffer` and `CallInput::Bytes`; local evm2 uses `message.input` directly because evm2 messages own their input bytes.
- Selector/count logic and `FourByteFrame` conversion are preserved.
- Documentation was shortened.

Assessment: no missing upstream behavior found for evm2's owned-input model.

### `src/tracing/js/bindings.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `SharedMemory`, `Stack`, `EvmState`, and `DatabaseRef` to evm2 `Memory`, `StackRef`, `Word`, and `CacheDB<EmptyDB>`.
- `MemoryRef` is not equivalent:
  - Upstream wraps a guarded reference to revm `SharedMemory`.
  - Local copies the full evm2 memory into owned `Bytes` before exposing it to JS.
  - JS behavior is preserved, but this changes lifetime/performance characteristics.
- `StackRef` is not equivalent:
  - Upstream wraps a guarded reference to revm `Stack`.
  - Local copies the stack into owned `Vec<Word>` before exposing it to JS.
  - JS `peek` still returns nth-from-top values, but snapshot/copy behavior differs.
- DB access is materially reduced:
  - Upstream has `StateRef`, `GcDb`, `EvmDbRefInner`, `JsDb<DB: DatabaseRef>`, and `StringError`; it reads first from in-flight `EvmState`, then from an arbitrary read-only `DatabaseRef`.
  - Local removed those pieces and exposes only a guarded `&CacheDB<EmptyDB>`.
  - Local `getCode` can only read code already present in `db.cache.contracts`; it cannot call `code_by_hash_ref`.
  - Local `getState` can only read cached storage entries from `db.cache.storage`; it cannot call `storage_ref`.
  - Local `exists`, `getBalance`, and `getNonce` only inspect local `CacheDB` account info.
- Local constant mapping changed from revm `KECCAK_EMPTY` to evm2 `KECCAK256_EMPTY`.
- Test setup was ported from revm `CacheDB + EvmState` to evm2 `CacheDB<EmptyDB>`.

Assessment: real JS tracer behavior gaps remain for database/state access. The JS DB object is cache-only, not the upstream read-only journal plus database view.

### `src/tracing/js/builtins.rs`

Status: reviewed.

Concrete differences:

- Diffs are import ordering and one formatter-driven semicolon change.
- Builtin registration and helper behavior match upstream: JSON conversion, `BigInt` JSON support, `toHex`, `toWord`, `toAddress`, `toContract`, `toContract2`, `slice`, precompile registration, and byte conversion helpers are preserved.

Assessment: no missing upstream behavior found.

### `src/tracing/js/mod.rs`

Status: reviewed.

Concrete differences:

- This file exists locally but is not the active JS module. `src/tracing/mod.rs` defines an inline `#[cfg(feature = "js-tracer")] pub mod js { ... }`, so Rust resolves active submodules through that inline module and does not compile `src/tracing/js/mod.rs`.
- The local file is a stale revm-shaped file with names rewritten toward evm2 (`DatabaseRef`, `ContextTr`, `CallInputs`, `CreateInputs`, `ResultAndState`, etc.). It does not match the actual evm2 inspector trait and is not wired into the crate.
- Upstream uses this file as the real JS inspector implementation. Local uses the inline JS implementation in `src/tracing/mod.rs` instead.

Assessment: the file itself is dead source in the current crate layout. The real behavior comparison for JS inspector logic is under `src/tracing/mod.rs`, `src/tracing/js/bindings.rs`, and `src/tracing/js/builtins.rs`.

### `src/tracing/mod.rs`

Status: reviewed.

Concrete differences:

- Ported the active tracing inspector from revm `ContextTr`, `JournalExt`, `CallInputs`, `CreateInputs`, `InterpreterResult`, and journal entries to evm2 `Inspector<T>`, `Evm<T>` host, `Message<T>`, `MessageResult<T>`, and `StateChanges`.
- Local file contains the active inline `js` module instead of `pub mod js;`.
- Active JS tracer is materially different from upstream:
  - Upstream `JsInspector::get_result` receives tx env, block env, `ResultAndState`, and DB, then passes in-flight state plus DB to the JS DB object.
  - Local exposes `json_result_from_parts` and `result_from_parts` over `JsTraceResult`, `JsTraceTx`, `JsTraceBlock`, and `&CacheDB<EmptyDB>`.
  - Local `step` and `fault` create a fresh empty `CacheDB<EmptyDB>` for the JS DB argument, so per-step JS database access sees an empty placeholder.
  - Local final `result` can use the provided `CacheDB<EmptyDB>`, but only with the cache-only limitations described in `src/tracing/js/bindings.rs`.
  - Precompile registration uses `Precompiles::base(interp.spec()).warm_addresses()` rather than the host's configured precompile provider.
- `TracingInspector` state is different:
  - Upstream tracks `record_step_end`, `last_call_return_data`, and `last_journal_len`.
  - Local tracks `step_stack` and `log_index`; it removed journal-length and last-return-data tracking.
- Step recording is not fully equivalent:
  - Upstream reuses prior memory snapshots when possible; local copies the current memory every recorded step.
  - Upstream computes immediate bytes through bytecode-aware `immediate_size(&interp.bytecode)`; local uses opcode-only `immediate_size(op.get())`, inheriting the dynamic-immediate gap noted in `src/opcode.rs`.
  - Upstream computes pushed stack items from opcode output count; local records stack slice growth from `stack_len_before`, which is a different heuristic.
- Storage diff recording is materially different:
  - Upstream uses the revm journal in `step_end` and records both `StorageChanged` and `StorageWarmed`, covering `SSTORE` changes and `SLOAD` warm-load observations.
  - Local does not fill storage changes during `step_end`.
  - Local adds `fill_storage_changes(&StateChanges)`, which post-processes recorded `SSTORE` steps using transaction-level `StateChanges` and recorded stack values.
  - Local cannot record upstream-style `SLOAD` warm-load storage changes from the journal.
- Call/create trace setup is ported but not mechanically identical:
  - Upstream derives create addresses from caller nonce in the journal during `create`.
  - Local starts create traces from `message.destination` and updates the trace address from `result.created_address` in `create_end`.
  - Upstream special-cases delegate-call value from the parent trace; local uses `message.value` and relies on evm2 `Message` to carry the effective value.
- Precompile exclusion was fixed to use the evm2 host:
  - Local `TracingInspector` is implemented for `T: EvmTypes<Host = Evm<T>>`.
  - `is_precompile_call` uses `host.precompiles().contains(&message.code_address)` plus `!message.disable_precompiles`, deep-call, and zero-value checks.
  - Excluded precompile calls use `PushTraceKind::PushOnly`, matching the upstream trace-tree shape.
- Log indexing differs:
  - Upstream uses global log count for `index` and `trace.children.len()` for `position`.
  - Local uses per-node log count for `position` and a separate global `log_index` for `index`.
- Upstream has no inline JS tests here; local adds evm2-native JS tests inside the inline module.
- `TransactionContext` is preserved, with wording-only doc changes.
- Upstream `CallInputExt` is removed because evm2 messages own input bytes.

Assessment: real gaps remain for JS DB visibility, journal-backed storage changes, dynamic immediate bytes, host-configured JS precompile lists, and exact step/log metadata parity.

### `src/tracing/mux.rs`

Status: reviewed.

Concrete differences:

- Ported from revm context/result/database types to evm2 `StateChanges`, `Evm<T>` host bound, and `Message<T>` hooks.
- Config parsing and shared `TracingInspectorConfig` merge behavior are preserved.
- `try_into_mux_frame` is not equivalent:
  - Upstream receives `ResultAndState` plus DB and passes DB into prestate traces.
  - Local receives `gas_used` and `StateChanges`; DB-backed gaps are inherited from `GethTraceBuilder`.
- Inspector delegation is manually expanded with evm2 hook signatures.
- Upstream delegates `log_full`, `frame_start`, and `frame_end`; local evm2 has no such hooks.

Assessment: behavior gaps are inherited from missing DB-backed builders and missing evm2 frame/log-full hooks.

### `src/tracing/opcount.rs`

Status: reviewed.

Concrete differences:

- Only functional change is porting the inspector signature from revm to evm2.
- Counter behavior is identical: increment once per `step`.

Assessment: no missing upstream behavior found.

### `src/tracing/types.rs`

Status: reviewed.

Concrete differences:

- Ported status fields from revm `InstructionResult` to evm2 `InstrStop`.
- `CallTrace.status` and `CallTraceStep.status` are now skipped by serde when the `serde` feature is enabled. Upstream serializes revm-compatible status values through its types.
- Success/error checks changed from revm `is_ok`/`is_revert` semantics to evm2 `InstrStop::is_success`/`is_revert`.
- Opcode imports and invalid-opcode fallback were ported from revm to evm2.
- `RecordedMemory` docs point at evm2 memory.
- Removed upstream conversions from revm `CallScheme` and `CreateScheme`; local defines `From<MessageKind> for CallKind` in `src/tracing/mod.rs`.
- Trace shape, call/log node types, Parity/GetH conversion helpers, `CallKind`, `TraceMemberOrder`, `DecodedTraceStep`, `StorageChange`, and `RecordedMemory` APIs are otherwise preserved.

Assessment: no missing data structures found, but serialized output differs because evm2 `InstrStop` status fields are skipped.

### `src/tracing/utils.rs`

Status: reviewed.

Concrete differences:

- Ported error formatting from revm `InstructionResult` to evm2 `InstrStop`.
- Removed upstream `load_account_code<DB: DatabaseRef>`. This is the helper upstream builders use to fetch bytecode through account code or code hash; its absence is the root of the DB-backed code gaps in geth/parity builders.
- Error message mapping is close but not identical:
  - Local has evm2-specific variants such as `PrecompileOOG`, `OutOfFunds`, `MemoryOOG`, `MemoryLimitOOG`, `InvalidOperandOOG`, `PrecompileError`, and `ReentrancySentryOOG`.
  - Upstream has revm-specific variants such as `InvalidFEOpcode` and maps that case to invalid opcode style messages.
  - Unmatched local statuses fall back to `format!("{status:?}")`.
- `convert_memory`, `gas_used`, and `maybe_revert_reason` behavior is preserved.
- Tests for revert reason decoding and memory chunk formatting are preserved with local naming/docs.

Assessment: real builder fidelity gap remains because `load_account_code` was removed with DB access.

### `src/tracing/writer.rs`

Status: reviewed.

Concrete differences:

- Ported status display from revm `InstructionResult::Stop` and `status.is_ok()` to evm2 `InstrStop::Stop` and `status.is_success()`.
- Imports and `num_or_hex` formatting differ only due to formatter/style.
- Trace writer configuration, call/log/step writing, decoded output handling, storage-change printing, color handling, and cheatcode coloring are otherwise preserved.

Assessment: no missing upstream behavior found.

### `src/opcode.rs`

Status: reviewed.

Concrete differences:

- Ported from revm context/interpreter types to evm2 `Inspector<T>`, `Interpreter<'_, T>`, `Message<T>`, and `MessageResult<T>`.
- Per-step opcode counting and gas measurement are structurally equivalent: record opcode and gas remaining in `step`, then add `gas_remaining - current_remaining` in `step_end`.
- Call/create gas-limit subtraction is preserved, but root-depth detection changed:
  - Upstream skips when `context.journal_ref().depth() == 0`.
  - Local evm2 port skips when `message.depth == 1`.
  - This appears to map to evm2's nested-message depth semantics, but it is not mechanically identical to upstream journal depth.
- Call/create opcode mapping is preserved for `CALL`, `CALLCODE`, `DELEGATECALL`, `STATICCALL`, `CREATE`, and `CREATE2`.
- `immediate_size` is not equivalent:
  - Upstream accepts `bytecode: &impl Immediates` and can account for bytecode-dependent immediates, specifically noted for `RJUMPV`.
  - Local evm2 port accepts only `opcode: u8` and returns `OpCode::immediate_size`, so it cannot inspect following bytes for dynamic immediate sizing.
- Tests were rewritten from revm interpreter setup to evm2 interpreter/host setup.

Assessment: potential behavior gap remains in `immediate_size` for dynamic immediate opcodes if evm2 supports them; root-depth skip should be kept in mind but matches current evm2 message-depth shape.

### `src/storage.rs`

Status: reviewed.

Concrete differences:

- Ported from revm to evm2 imports and inspector signatures.
- `SLOAD` detection and slot counting are preserved.
- Stack access changed from revm `interp.stack.peek(0)` to evm2 `interp.stack().peekn()`.
- Address tracked for storage access changed from revm `interp.input.target_address()` to evm2 `interp.message().destination`.
- Public API is present: `new`, `unique_loads`, `warm_loads`, `accessed_slots`, and `into_accessed_slots`.

Assessment: no missing upstream behavior found, assuming evm2 `message.destination` is the intended storage context for delegated execution.

### `src/tracing/arena.rs`

Status: reviewed.

Concrete differences:

- File is identical to upstream.

Assessment: no missing upstream behavior found.

### `src/tracing/builder/mod.rs`

Status: reviewed.

Concrete differences:

- File is identical to upstream.

Assessment: no missing upstream behavior found.

### `src/tracing/builder/walker.rs`

Status: reviewed.

Concrete differences:

- File is identical to upstream.

Assessment: no missing upstream behavior found.

### `src/lib.rs`

Status: reviewed.

Concrete differences:

- Crate docs changed from revm to evm2.
- Local crate adds dummy dependency imports guarded by features to satisfy `warn(unused_crate_dependencies)`.
- Module set matches upstream: `access_list`, `edge_cov`, `opcode`, `storage`, `tracing`, and `transfer`.
- Local crate additionally re-exports `OpcodeGasInspector` and `immediate_size`.

Assessment: no missing upstream behavior found.

### `src/transfer.rs`

Status: reviewed.

Concrete differences:

- Ported from revm `ContextTr`, `JournalTr`, `CallInputs`, `CreateInputs`, `CreateOutcome`, and `CreateScheme` to evm2 `Inspector<T>`, `Message<T>`, `MessageResult<T>`, and `MessageKind`.
- Public API changed around logs:
  - Upstream `with_logs(true)` inserts ERC20-style transfer logs directly into the revm journal via `journaled_state.log(...)`.
  - Local `with_logs(true)` stores logs in `TransferInspector::logs` and exposes them through `logs()`.
  - This means local logs are not automatically part of EVM execution logs unless the caller merges them.
- Call transfer detection is ported but not mechanically identical:
  - Upstream uses `inputs.transfer_value()`, `transfer_from()`, and `transfer_to()`.
  - Local records only `MessageKind::Call | MessageKind::CallCode` with `message.caller`, `message.destination`, and `message.value`.
  - Zero-value transfers are still skipped.
- Create transfer timing differs:
  - Upstream records create/create2 transfers in `create`, deriving the created address from caller nonce before execution.
  - Local records create/create2 transfers in `create_end` only when `result.created_address` is present.
  - Failed creates or creates without a produced address therefore differ from upstream's attempted-transfer recording.
- `internal_only` top-level filtering changed from revm journal depth to evm2 `message.depth`.
- Local `TransferOperation` and `TransferKind` are `Copy`; upstream types are clone-only. This is API-only, not behavior.
- Selfdestruct transfer recording is otherwise preserved, including direct recording without zero-value filtering or synthetic log insertion.

Assessment: real behavior differences remain for synthetic log insertion and failed/attempted create transfer recording.
