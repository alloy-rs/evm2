# Diff Reduction Tracker

Baseline: `/home/doni/github/paradigmxyz/revm-inspectors` at `6566b83`.

Port: `/home/doni/github/danipopes/evm2.1/crates/inspectors`.

Generator: `scripts/generate_inspectors_diffs.sh`.

Rule set:
- Restore upstream docs/comments unless they are misleading after the evm2 port.
- Ignore import ordering and normal rustfmt formatting.
- Keep clippy-driven `const fn` upgrades.
- Keep upstream expression patterns (`and_then`, `or_else`, `map_err`, etc.) where they still fit.
- Keep doc comments before attributes.
- Track function/API signature differences in `diffs/SIGNATURE_DIFFERENCES.md`.

Status legend:
- `[ ]`: not reviewed in this reduction pass.
- `[~]`: reviewed, intentional diff remains.
- `[x]`: reduced as far as reasonable.
- `[!]`: needs code follow-up.

## Non-Empty Diffs In Generated Order

- [~] `Cargo.toml` -> `diffs/Cargo.toml.diff`
  - Workspace metadata/dependency substitutions are intentional. No reduction target besides package identity.
- [~] `README.md` -> `diffs/README.md.diff`
  - Crate-name/link substitutions only.
- [x] `src/access_list.rs` -> `diffs/src__access_list.rs.diff`
  - Restored upstream docs/comments and removed a port-only helper. Remaining diff is evm2 hook/state substitution plus clippy const getters.
- [x] `src/lib.rs` -> `diffs/src__lib.rs.diff`
  - Restored feature/module docs where still accurate. Remaining diff is crate identity, dependency keepalive imports, and intentional edge coverage omission.
- [x] `src/opcode.rs` -> `diffs/src__opcode.rs.diff`
  - Restored upstream comments where applicable. Remaining diff is evm2 hook/test harness substitution and `immediate_size(u8)` because evm2 does not expose bytecode immediate decoding/RJUMPV.
- [x] `src/storage.rs` -> `diffs/src__storage.rs.diff`
  - Removed port-only module doc and restored upstream control-flow shape. Remaining diff is evm2 opcode/stack/message substitution plus clippy punctuation.
- [~] `src/tracing/arena.rs` -> `diffs/src__tracing__arena.rs.diff`
  - Clippy-driven `const fn` only.
- [x] `src/tracing/builder/geth.rs` -> `diffs/src__tracing__builder__geth.rs.diff`
  - Restored upstream comments around prestate/state-diff handling. Remaining diff is evm2 `StateChanges` reconstruction, DB error propagation, `spec_id` for selfdestruct diff behavior, and port-specific tests for code/error cases.
- [x] `src/tracing/builder/parity.rs` -> `diffs/src__tracing__builder__parity.rs.diff`
  - Restored upstream comments/docs where applicable. Remaining diff is evm2 output/state/db plumbing and `StateChanges`-based state-diff population.
- [x] `src/tracing/config.rs` -> `diffs/src__tracing__config.rs.diff`
  - Reviewed. Remaining diff is evm2 opcode import, clippy const methods, and prestate config enabling the data needed for evm2 reconstruction.
- [x] `src/tracing/debug.rs` -> `diffs/src__tracing__debug.rs.diff`
  - Restored delegate macro for hooks where type inference permits it. Remaining explicit `log`/`selfdestruct` dispatch is needed for evm2 trait defaults; frame/log_full absence is expected.
- [x] `src/tracing/fourbyte.rs` -> `diffs/src__tracing__fourbyte.rs.diff`
  - Reviewed. Remaining diff is evm2 message calldata access plus comment punctuation/import formatting.
- [x] `src/tracing/js/bindings.rs` -> `diffs/src__tracing__js__bindings.rs.diff`
  - Restored upstream docs/comments and test clone shapes where they compile. Remaining diff is evm2 memory/stack/state/db reader infrastructure; manual guard `Clone` is required for non-Clone evm2 values.
- [x] `src/tracing/js/builtins.rs` -> `diffs/src__tracing__js__builtins.rs.diff`
  - Reduced to imports/rustfmt and minor borrow differences.
- [x] `src/tracing/js/mod.rs` -> `diffs/src__tracing__js__mod.rs.diff`
  - Restored upstream module docs, field docs, helper docs, and selfdestruct comments. Remaining diff is evm2 tx/result/message plumbing and helper shape.
- [x] `src/tracing/mod.rs` -> `diffs/src__tracing__mod.rs.diff`
  - Restored public `with_transaction_gas_limit` and upstream docs/comments. Remaining diff is evm2 hook model, explicit step stack, log index, storage journal scan, and intentional deprecated getter/reusable step vec omissions.
- [x] `src/tracing/mux.rs` -> `diffs/src__tracing__mux.rs.diff`
  - Reviewed. Remaining diff is evm2 hook signatures and direct gas/state/db builder inputs.
- [x] `src/tracing/opcount.rs` -> `diffs/src__tracing__opcount.rs.diff`
  - Restored upstream built-in tracer doc link. Remaining diff is evm2 hook signature and clippy punctuation.
- [x] `src/tracing/types.rs` -> `diffs/src__tracing__types.rs.diff`
  - Reviewed. Remaining diff is `InstrStop` substitution, evm2 opcode names, clippy constness/Self usage, and removed revm-only conversion impls.
- [x] `src/tracing/utils.rs` -> `diffs/src__tracing__utils.rs.diff`
  - Restored upstream `load_account_code` docs and repo link. Remaining diff is evm2 `InstrStop`/`SpecId`/DB error propagation.
- [x] `src/tracing/writer.rs` -> `diffs/src__tracing__writer.rs.diff`
  - Reviewed. Remaining diff is `InstrStop` substitution, clippy const methods, and rustfmt expression formatting.
- [x] `src/transfer.rs` -> `diffs/src__transfer.rs.diff`
  - Restored upstream transfer docs/comments. Remaining diff is evm2 message/log hook plumbing and clippy const constructors.
- [x] `tests/it/accesslist.rs` -> `diffs/tests__it__accesslist.rs.diff`
  - Reviewed. Same scenario with evm2 harness substitutions.
- [x] `tests/it/geth.rs` -> `diffs/tests__it__geth.rs.diff`
  - Reviewed. Upstream tests are present; differences are evm2 harness/result substitutions and prestate DB mutability.
- [x] `tests/it/geth_js.rs` -> `diffs/tests__it__geth_js.rs.diff`
  - Reviewed. Upstream tests are present; additional debug-inspector JS regression documents the ported dispatcher path.
- [x] `tests/it/main.rs` -> `diffs/tests__it__main.rs.diff`
  - Reviewed. Edge coverage is intentionally omitted; js tests require both `std` and `js-tracer`.
- [x] `tests/it/parity.rs` -> `diffs/tests__it__parity.rs.diff`
  - Reviewed. Upstream parity tests are present with evm2 harness/result substitutions.
- [x] `tests/it/repro/mod.rs` -> `diffs/tests__it__repro__mod.rs.diff`
  - Reviewed. Hardfork mapping uses `alloy_hardforks` as requested and falls back to `SpecId::NEXT`.
- [x] `tests/it/repro/prestate.rs` -> `diffs/tests__it__repro__prestate.rs.diff`
  - Reviewed. Same repro checks with evm2 prestate DB plumbing.
- [x] `tests/it/test_native_bigint.rs` -> `diffs/tests__it__test_native_bigint.rs.diff`
  - Reviewed. Import ordering only.
- [x] `tests/it/transfer.rs` -> `diffs/tests__it__transfer.rs.diff`
  - Reviewed. Upstream transfer test is present; extra failed-CREATE regression covers evm2 create-message behavior.
- [x] `tests/it/utils.rs` -> `diffs/tests__it__utils.rs.diff`
  - Reviewed. Large intentional evm2 test harness replacement; removed redundant `TracingInspectorExt` after restoring the real public method.
- [x] `tests/it/writer.rs` -> `diffs/tests__it__writer.rs.diff`
  - Reviewed. Same snapshot tests with evm2 harness imports.

## Identical Files

Empty `.diff` files are no longer written. Files without a generated diff artifact are byte-for-byte identical to upstream or intentionally missing in the port inventory tracked by `diffs/REVIEW_TODO.md`.
