# Fresh Upstream Diff Review Todo

Baseline: `/home/doni/github/paradigmxyz/revm-inspectors` at `3895078`.

Port: `/home/doni/github/danipopes/evm2.1/crates/inspectors`.

Diff artifacts: `./diffs/*.diff` for non-empty diffs only.

Legend: `[ ]` pending, `[x]` reviewed, `[!]` needs follow-up.

## Source and Package Files

- [x] `Cargo.toml` -> `diffs/Cargo.toml.diff`
  - Workspace metadata/dependencies/features are expected evm2 substitutions; no API concern.
- [x] `README.md` -> `diffs/README.md.diff`
  - Name/link substitution only.
- [x] `src/access_list.rs` -> `diffs/src__access_list.rs.diff`
  - API and opcode collection match; exclusions use evm2 top-level message/precompile/7702 data.
- [x] `src/edge_cov.rs` -> missing in port
  - Intentionally omitted with edge coverage.
- [x] `src/lib.rs` -> `diffs/src__lib.rs.diff`
  - Public modules match except intentional `edge_cov` omission; dependency keepalive imports only.
- [x] `src/opcode.rs` -> `diffs/src__opcode.rs.diff`
  - Semantics match after evm2 message substitutions; `immediate_size(u8)` difference is expected because RJUMPV decoding is not implemented.
- [x] `src/storage.rs` -> `diffs/src__storage.rs.diff`
  - SLOAD address/slot counting maps cleanly to evm2 message destination and stack access.
- [x] `src/tracing/arena.rs` -> `diffs/src__tracing__arena.rs.diff`
  - Only `nodes_mut` constness differs.
- [x] `src/tracing/builder/geth.rs` -> `diffs/src__tracing__builder__geth.rs.diff`
  - Geth builders preserve upstream call/default/ERC-7562 behavior with evm2 types. Prestate differs structurally because evm2 exposes `StateChanges` instead of revm's journaled state map, so touched accounts and storage are reconstructed from trace steps/state changes. Post-code loading now checks `StateChanges::code` by code hash before DB fallback.
- [x] `src/tracing/builder/mod.rs`
  - No diff artifact; byte-for-byte identical.
- [x] `src/tracing/builder/parity.rs` -> `diffs/src__tracing__builder__parity.rs.diff`
  - Trace/vmTrace construction matches upstream. StateDiff population maps to `StateChanges` plus pre-state DB reads, with output bytes passed directly because evm2 result plumbing already split them out.
- [x] `src/tracing/builder/walker.rs`
  - No diff artifact; byte-for-byte identical.
- [x] `src/tracing/config.rs` -> `diffs/src__tracing__config.rs.diff`
  - Only evm2 opcode import/constness and prestate config forcing steps/state-diffs/stack snapshots so prestate can be reconstructed without revm's state map.
- [x] `src/tracing/debug.rs` -> `diffs/src__tracing__debug.rs.diff`
  - Dispatcher behavior matches upstream tracers after evm2 result/tx/block/db substitutions. `Noop` is a unit variant because there is no stored revm noop inspector, and missing frame/log_full hooks are expected.
- [x] `src/tracing/fourbyte.rs` -> `diffs/src__tracing__fourbyte.rs.diff`
  - Selector/calldata counting matches; evm2 messages already carry materialized calldata.
- [x] `src/tracing/js/bindings.rs` -> `diffs/src__tracing__js__bindings.rs.diff`
  - JS object surface is preserved, including upstream stack/memory index validation and owned
    pre-step snapshots. Internal DB access is split into in-flight `State` reads and final
    `StateChanges` reads because evm2 lacks revm's `EvmState + DatabaseRef` pairing; the extra
    reader trait is private binding infrastructure, not public API.
- [x] `src/tracing/js/builtins.rs` -> `diffs/src__tracing__js__builtins.rs.diff`
  - Builtin semantics and tests match, including direct BigInt construction and geth `bigInt`
    compatibility shims.
- [x] `src/tracing/js/mod.rs` -> `diffs/src__tracing__js__mod.rs.diff`
  - Hook behavior maps to evm2 messages/results; call stack, deferred step/fault callbacks,
    pre-step stack/memory snapshots, result objects, precompile registration, runtime limits, and
    error-to-revert behavior match upstream. Delegatecall value has already been fixed to upstream
    semantics.
- [x] `src/tracing/mod.rs` -> `diffs/src__tracing__mod.rs.diff`
  - Core tracing semantics match with evm2 hooks: root trace starts in `initialize_interp`, step bookkeeping uses an explicit stack, logs use a global index, storage changes scan new journal entries, and precompile exclusion uses evm2 precompile/message data. Deprecated getters are intentionally absent; the reusable step vec pool is restored.
- [x] `src/tracing/mux.rs` -> `diffs/src__tracing__mux.rs.diff`
  - Mux config and output assembly match upstream. Differences are evm2 inspector hook signatures and passing gas/state/db directly into underlying builders.
- [x] `src/tracing/opcount.rs` -> `diffs/src__tracing__opcount.rs.diff`
  - Opcode step counting is equivalent.
- [x] `src/tracing/types.rs` -> `diffs/src__tracing__types.rs.diff`
  - Data shape matches after `InstrStop` substitution; removed revm-only conversion impls are replaced in `tracing/mod.rs`.
- [x] `src/tracing/utils.rs` -> `diffs/src__tracing__utils.rs.diff`
  - Error/gas/revert helpers are equivalent after evm2 substitutions; `load_account_code` now propagates DB errors.
- [x] `src/tracing/writer.rs` -> `diffs/src__tracing__writer.rs.diff`
  - Writer output logic matches upstream, including receive/fallback display and last-write storage
    ordering; remaining changes are `InstrStop` substitution, constness, formatting, and
    `is_success()` naming.
- [x] `src/transfer.rs` -> `diffs/src__transfer.rs.diff`
  - Transfer recording matches upstream for call/create/selfdestruct after evm2 message/log substitutions.

## Integration Tests and Fixtures

- [x] `testdata/Counter.sol`
  - No diff artifact; byte-for-byte identical.
- [x] `testdata/repro/tx-selfdestruct.json`
  - No diff artifact; byte-for-byte identical.
- [x] `tests/it/accesslist.rs` -> `diffs/tests__it__accesslist.rs.diff`
  - Same test scenario; harness rewritten to evm2 transaction helper.
- [x] `tests/it/edge_cov.rs` -> missing in port
  - Intentionally omitted with edge coverage. This is the only upstream test name absent from the port.
- [x] `tests/it/geth.rs` -> `diffs/tests__it__geth.rs.diff`
  - All upstream geth tests are present with evm2 harness/result substitutions. Assertions are preserved; prestate/state-diff expected values account for evm2 state reconstruction and prior fixed code-loading behavior.
- [x] `tests/it/geth_js.rs` -> `diffs/tests__it__geth_js.rs.diff`
  - Upstream JS tracer tests are present with evm2 harness substitutions. One additional debug-inspector JS tracer regression covers the ported dispatcher path.
- [x] `tests/it/main.rs` -> `diffs/tests__it__main.rs.diff`
  - Same integration module set except intentional `edge_cov` removal and shared evm2 utils module.
- [x] `tests/it/parity.rs` -> `diffs/tests__it__parity.rs.diff`
  - All upstream parity tests are present with evm2 harness/result substitutions. Assertions and fixture coverage are preserved.
- [x] `tests/it/repro/mod.rs` -> `diffs/tests__it__repro__mod.rs.diff`
  - Fixture loading/prestate conversion preserved; hardfork mapping uses alloy as requested and maps to evm2 `SpecId`.
- [x] `tests/it/repro/prestate.rs` -> `diffs/tests__it__repro__prestate.rs.diff`
  - Selfdestruct prestate repro tests are present with evm2 tx helper substitutions.
- [x] `tests/it/test_native_bigint.rs` -> `diffs/tests__it__test_native_bigint.rs.diff`
  - Empty semantic diff; module import path adjusted.
- [x] `tests/it/transfer.rs` -> `diffs/tests__it__transfer.rs.diff`
  - Upstream transfer test preserved with evm2 harness substitutions. Added regression for failed CREATE transfer attempt.
- [x] `tests/it/utils.rs` -> `diffs/tests__it__utils.rs.diff`
  - This is intentionally the largest test-only divergence: it replaces revm harness helpers with evm2 transaction/block/state helpers. No production API is introduced here.
- [x] `tests/it/writer.rs` -> `diffs/tests__it__writer.rs.diff`
  - Same writer tests and snapshot comparison logic; only path/harness substitutions.

All `tests/it/writer/**` snapshot files are byte-for-byte unchanged from upstream and no longer generate empty diff artifacts.

## MANUAL

- src__tracing__builder__geth.rs.diff
  - `spec_id` is intentionally threaded into the geth builder so diff-mode prestate can apply
    Cancun selfdestruct behavior while evm2 `StateChanges` lacks revm's journal state shape.
- src__tracing__builder__parity.rs.diff
  - State diff and vmTrace bytecode population use evm2 `StateChanges` plus mutable DB reads; the
    helper API shape differs from upstream `DatabaseRef` but preserves output construction.
- src__tracing__config.rs.diff
  - `from_geth_prestate_config` intentionally enables steps/state diffs/stack snapshots because
    evm2 prestate reconstruction needs opcode-touched accounts and storage reads.
- src__tracing__js__bindings.rs.diff
  - DB access is intentionally split between in-flight `State` reads and post-transaction
    `StateChanges` reads because evm2 does not expose revm's `EvmState + DatabaseRef` pairing.
- src__tracing__js__mod.rs.diff
  - Result status handling maps revm `ExecutionResult` to evm2 `TxResult`/`InstrStop`; JS result
    surface stays aligned with geth tracer expectations.
  - fn try_* ordering addressed in cleanup pass
  - SharedJsInspector removed in cleanup pass
  - test indentation/order addressed in cleanup pass
- src__tracing__mod.rs.diff
  - reusable_step_vecs restored in cleanup pass
  - `start_trace` address matching preserves upstream call/delegatecall/callcode caller/address
    semantics using evm2 `MessageKind`.
- src__tracing__utils.rs.diff
  - fn gas_used keeps evm2 `SpecId::enables`; upstream-shaped `is_enabled_in` is deprecated here

## Cleanup Pass

- [x] `src/tracing/utils.rs`
  - Long `hex!` test literals match upstream semantically, but `cargo fmt --all` keeps them
    multiline in this repo.
- [x] `src/tracing/mod.rs`
  - Restored upstream public helper ordering around transaction setters/builders where evm2
    signatures still require different call arguments.
- [x] `src/tracing/js/mod.rs`
  - Restored upstream raw-string indentation, nearby test comments, and test ordering while keeping
    evm2 harness substitutions.
- [x] `src/tracing/js/mod.rs`
  - Restored private `try_fault`/`try_step`/`try_enter`/`try_exit` helper ordering and moved
    `push_call` back after the call-stack predicate helpers.
- [x] `src/tracing/js/mod.rs`
  - Removed the test-only `SharedJsInspector` wrapper; tests now recover `JsInspector` with
    `Evm::clear_inspector_as` after execution.
- [x] `src/tracing/mod.rs`
  - Restored upstream reusable empty `CallTraceStep` vector pool across `fuse()` and `start_trace`.
- [x] `src/tracing/mod.rs`
  - Restored upstream `#[inline]` annotations on `fuse` and `fused`.
- [x] `src/tracing/js/mod.rs`
  - Restored upstream `Default::default()` initialization for `call_stack`.
- [x] `src/tracing/js/bindings.rs`
  - Restored upstream wording for `MemorySnapshot` docs and enum variant qualification in
    `Guarded::as_ref`.
- [x] `src/tracing/mod.rs`
  - Restored upstream-style local `use types::{CallLog, CallTrace, CallTraceStep};`.
- [x] `src/tracing/js/mod.rs`
  - Folded `register_builtins` back into the `js::builtins` import group and restored
    `TransactionContext` import placement.
- [x] `src/tracing/js/builtins.rs`
  - Restored upstream order for BigInt compatibility tests and explanatory geth-pattern comments.
- [x] `tests/it/writer.rs`
  - Restored removed receive/fallback regression comments while keeping local punctuation style.
- [x] `tests/it/geth_js.rs`
  - Moved the additional debug-inspector JS tracer regression after the upstream tests.
- [x] `src/tracing/debug.rs`
  - Restored upstream JS tracer config temporary while keeping evm2 result handling.
- [x] `src/tracing/mod.rs`
  - Restored upstream `TransactionContext` documentation text.
- [x] `src/tracing/js/mod.rs`
  - Restored upstream transaction-context doc note and test byte literal macro usage.
