# Porting Log

- `evm2-inspectors` keeps the upstream `js-tracer` feature name and compiles the pure JS builtins,
  native BigInt tests, and constructor-only JS inspector tests, but the full JS inspector remains
  disabled. The JS tracer implementation still depends on revm-specific context, database,
  journal, and interpreter APIs that do not exist in evm2 yet.
  This intentionally disables the full `JsInspector` inspection hooks and `tests/it/geth_js.rs`.
  Porting them faithfully requires replacing revm's
  `ContextTr`/`JournalTr`/`DatabaseRef`/`JournalExt`-backed inspector context and direct
  `InterpreterAction`/`InterpreterResult` control with evm2 equivalents or exposing additional
  evm2 host/journal/database state to inspectors, which is a larger core API change.
