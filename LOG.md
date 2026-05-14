# Porting Log

- `evm2-inspectors` keeps the upstream `js-tracer` feature name, but the feature is a no-op for
  now. The JS tracer implementation still depends on revm-specific context, database, journal, and
  interpreter APIs that do not exist in evm2 yet.
  This intentionally disables `tracing::js`, `tests/it/geth_js.rs`, and
  `tests/it/test_native_bigint.rs`. Porting them faithfully requires replacing revm's
  `ContextTr`/`JournalTr`/`DatabaseRef`/`JournalExt`-backed inspector context and direct
  `InterpreterAction`/`InterpreterResult` control with evm2 equivalents or exposing additional
  evm2 host/journal/database state to inspectors, which is a larger core API change.
