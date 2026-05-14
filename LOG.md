# Porting Log

- `evm2-inspectors` keeps the upstream `js-tracer` feature name, but the feature is a no-op for
  now. The JS tracer implementation still depends on revm-specific context, database, journal, and
  interpreter APIs that do not exist in evm2 yet.
