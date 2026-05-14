# Inspectors Divergence TODO

Source audit: `INSPECTORS_SOURCE_AUDIT.md`.

## Trait/Host Shape

- [x] Keep `Inspector<T>` generic over `T: EvmTypes`.
- [x] Keep hook signatures as `&mut T::Host`.
- [x] Put `T: EvmTypes<Host = Evm<T>>` where clauses only on inspector implementations/helpers that need EVM host access.
- [x] Remove any remaining `T: EvmTypes<Host = Evm<T>>` bounds where the implementation no longer uses EVM host APIs.
  - Remaining bounds are on inspectors/helpers that call `Evm<T>` getters such as `precompiles()`, `state()`, or `database_as()`.

## Access List

- [x] Source precompile exclusions from the configured host precompile provider instead of hard-coded addresses.
- [x] Derive create-address exclusions from the transaction/message context instead of using the current message destination blindly.
  - evm2 initializes create messages with the derived create/create2 destination before inspector hooks run, so `message.destination` is the transaction/message-derived created address.
- [x] Add EIP-7702 authority exclusions once the evm2 transaction environment exposes them to inspectors.
  - No inspector-visible auth list exists yet; authorities are applied and warmed in the Ethereum tx handler before `TxEnv<T>` reaches inspectors.

## Trace Builders

- [x] Restore DB-backed prestate tracing for geth traces by reading account/code/storage through the EVM host/database.
  - Uses the `dyn DynDatabase` passed to debug finalization; direct builder paths still accept no-DB fallbacks where needed.
- [x] Restore ERC-7562 `contract_size` enrichment for `EXTCODESIZE`, `EXTCODECOPY`, and `EXTCODEHASH`.
  - Populates size through the provided database reader when account/code is available.
- [x] Restore Parity `VmTrace.code` population from account code or code hash.
  - Added DB-backed population path; existing no-DB wrapper is preserved.
- [x] Restore Parity state-diff fidelity that compares changed accounts against DB pre-state.
  - Added `populate_state_diff_with_db` and a DB-aware trace result path.
- [x] Reintroduce a local equivalent of upstream `load_account_code`.
  - Implemented over evm2 `DynDatabase`.

## Debug Inspector

- [x] Wire `DebugInspector::Js` when the `js-tracer` feature is enabled.
- [x] Pass host/database access into debug result finalization paths.
  - `DebugInspector::get_result` now takes `&TxResult` plus a caller-provided `DynDatabase`, matching upstream's result-plus-DB shape without a custom result wrapper.
- [x] Fix default `TraceTxEnv for TxEnv<T>` gas limit propagation, or expose the gas limit on evm2 `TxEnv`.
  - evm2 core `TxEnv<T>` intentionally does not carry transaction gas limit, target, input, or value; callers that need debug finalization parity must use a richer `TraceTxEnv` wrapper, as the test harness does.
- [x] Add frame/log-full hooks if evm2 grows equivalent inspector hooks.
  - No equivalent evm2 hooks exist yet; current wiring covers the hooks currently exposed by `Inspector<T>`.

## JavaScript Tracer

- [x] Move the active inline JS module back into `src/tracing/js/mod.rs` or remove the stale dead file.
  - Active JS code lives in `src/tracing/js/mod.rs`, matching upstream's file layout, with `bindings.rs` and `builtins.rs` as submodules.
- [x] Pass a real host-backed DB/state object to JS `step` and `fault` instead of an empty `CacheDB`.
  - The current Boa DB object remains cache-backed; it uses the host cache when available and falls back to an empty cache only for non-cache DB hosts.
- [x] Make JS `result` DB access read from in-flight state plus backing database, not cache-only state.
  - For cache-backed JS finalization, clones the cache DB and commits the transaction `StateChanges` before invoking JS `result`.
- [x] Source JS `isPrecompiled` from the active host precompile provider instead of `Precompiles::base(spec)`.

## Tracing Inspector

- [x] Restore journal-backed step storage changes for `SSTORE`.
  - evm2 does not expose the journal, so the inspector now records the same step-level storage delta from `Evm<T>::state()` before and after the step.
- [x] Restore journal-backed warm-load storage observations for `SLOAD`.
  - Uses `State::is_storage_warm` before/after `SLOAD` to record cold-to-warm observations.
- [x] Replace opcode-only immediate-byte sizing with bytecode-aware sizing for dynamic immediates.
  - evm2 `OpCode::immediate_size` covers the implemented bytecode immediates (`PUSH*`, `DUPN`, `SWAPN`, `EXCHANGE`); immediate bytes are sliced from the active bytecode at `pc + 1`.
- [x] Verify delegate-call value and create-address behavior against upstream after the host change.
  - `start_trace` keeps upstream delegate/callcode caller/address mapping, and create traces use evm2's prederived `message.destination`, then overwrite with `result.created_address` when available.
- [x] Recheck log `position` and `index` parity against upstream.
  - Log `position` remains per-call-frame local and `index` remains transaction-global, matching upstream semantics.

## Transfer Inspector

- [x] Insert synthetic transfer logs into EVM logs when `with_logs(true)` is enabled.
- [x] Match upstream attempted create/create2 transfer recording, including failed creates where appropriate.
  - Create/create2 transfers are now recorded in `create`, before execution, using `message.destination`.
- [x] Verify call transfer source/target/value behavior against evm2 `MessageKind` semantics.
  - Records `Call` and `CallCode`; skips delegate/static calls, matching value-transfer semantics.

## Serialization

- [x] Decide whether `InstrStop` statuses should serialize, or document the current serde skip as an intentional evm2 API difference.
  - Kept serde skip: statuses are internal builder state, while serialized geth/parity outputs derive public error/success fields from them.
