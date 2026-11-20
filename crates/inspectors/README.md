# evm2-inspectors

Common [`evm2`] inspector implementations.

Originally:
- `revm-inspectors`
- part of `reth` as the `reth-revm-inspectors` crate.

Enable the `raw-struct-logs` Cargo feature to use the default geth tracer's
JSON output. Construct the inspector with `DebugInspector::new_with_struct_log_mode`
and `StructLogMode::Raw`, then serialize the frame returned by `get_trace`.
The recording budget counts the JSON bytes of the individual struct logs.
