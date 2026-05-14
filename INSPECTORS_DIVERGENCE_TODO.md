# Inspectors Divergence TODO

Source audit: `INSPECTORS_SOURCE_AUDIT.md`.

## Trait/Host Shape

- [x] Keep `Inspector<T>` generic over `T: EvmTypes`.
- [x] Keep hook signatures as `&mut T::Host`.
- [x] Put `T: EvmTypes<Host = Evm<T>>` where clauses only on inspector implementations/helpers that need EVM host access.
- [ ] Remove any remaining `T: EvmTypes<Host = Evm<T>>` bounds where the implementation no longer uses EVM host APIs.

## Access List

- [x] Source precompile exclusions from the configured host precompile provider instead of hard-coded addresses.
- [ ] Derive create-address exclusions from the transaction/message context instead of using the current message destination blindly.
- [ ] Add EIP-7702 authority exclusions once the evm2 transaction environment exposes them to inspectors.

## Trace Builders

- [ ] Restore DB-backed prestate tracing for geth traces by reading account/code/storage through the EVM host/database.
- [ ] Restore ERC-7562 `contract_size` enrichment for `EXTCODESIZE`, `EXTCODECOPY`, and `EXTCODEHASH`.
- [ ] Restore Parity `VmTrace.code` population from account code or code hash.
- [ ] Restore Parity state-diff fidelity that compares changed accounts against DB pre-state.
- [ ] Reintroduce a local equivalent of upstream `load_account_code`.

## Debug Inspector

- [x] Wire `DebugInspector::Js` when the `js-tracer` feature is enabled.
- [x] Pass host/database access into debug result finalization paths.
  - `DebugTraceResult::with_db` now exposes the caller's `CacheDB<EmptyDB>` to JS `result`; generalized host/state overlay DB access is still tracked under JavaScript Tracer.
- [ ] Fix default `TraceTxEnv for TxEnv<T>` gas limit propagation, or expose the gas limit on evm2 `TxEnv`.
  - Test harness `TxEnv` now propagates kind/input/gas price/value into JS debug finalization; core `TxEnv<T>` still does not carry transaction gas limit, target, input, or value.
- [ ] Add frame/log-full hooks if evm2 grows equivalent inspector hooks.

## JavaScript Tracer

- [ ] Move the active inline JS module back into `src/tracing/js/mod.rs` or remove the stale dead file.
- [ ] Pass a real host-backed DB/state object to JS `step` and `fault` instead of an empty `CacheDB`.
- [ ] Make JS `result` DB access read from in-flight state plus backing database, not cache-only state.
- [x] Source JS `isPrecompiled` from the active host precompile provider instead of `Precompiles::base(spec)`.

## Tracing Inspector

- [ ] Restore journal-backed step storage changes for `SSTORE`.
- [ ] Restore journal-backed warm-load storage observations for `SLOAD`.
- [ ] Replace opcode-only immediate-byte sizing with bytecode-aware sizing for dynamic immediates.
- [ ] Verify delegate-call value and create-address behavior against upstream after the host change.
- [ ] Recheck log `position` and `index` parity against upstream.

## Transfer Inspector

- [x] Insert synthetic transfer logs into EVM logs when `with_logs(true)` is enabled.
- [ ] Match upstream attempted create/create2 transfer recording, including failed creates where appropriate.
- [ ] Verify call transfer source/target/value behavior against evm2 `MessageKind` semantics.

## Serialization

- [ ] Decide whether `InstrStop` statuses should serialize, or document the current serde skip as an intentional evm2 API difference.
