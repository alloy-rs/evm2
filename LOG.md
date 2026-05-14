# Porting Log

- `evm2-inspectors` keeps the upstream `js-tracer` feature name and compiles the pure JS builtins,
  native BigInt tests, JS binding object tests, constructor tests, and step/result runtime unit
  tests. The active `JsInspector` now drives JavaScript `step`, `fault`, `enter`, and `exit` hooks
  from evm2's existing inspector callbacks, but the per-step `db` object is still backed by an
  empty in-memory database because evm2 inspector hooks do not expose the live database or journal.
  The upstream `tests/it/geth_js.rs` coverage is enabled because those cases do not depend on
  per-step database reads. Full DB-backed JS tracer semantics still require replacing revm's
  `ContextTr`/`JournalTr`/`DatabaseRef`/`JournalExt`-backed inspector context and direct
  `InterpreterAction`/`InterpreterResult` control with evm2 equivalents or exposing additional
  evm2 host/journal/database state to inspectors, which is a larger core API change.
