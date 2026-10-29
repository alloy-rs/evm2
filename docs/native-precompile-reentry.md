# Native precompile reentry

A native precompile can call a contract that calls another native precompile.
Both calls use the same provider and the original transaction environment.
Dispatch must therefore allow provider reentry while lending the child the host
EVM, including its journal and gas tracker.

Previously, `execute_precompile` derived an exclusive provider reference from
inside the host and passed it alongside an exclusive reference to the host.
Nested dispatch derived another exclusive reference to that provider. The
existing `precompile_can_call_another_precompile` test reproduces this aliasing
violation under Miri at commit
`897e237fc822cfd7bc03eb946c10b953f1c05a6f`.

The provider now lives behind a private shared handle. Dispatch retains a clone
outside the host before borrowing the host and calls `PrecompileProvider::execute`
through `&self`. Configuration mutation remains available while execution is
idle, after dispatch handles have been released. Implementors must change their
execution receiver from `&mut self` to `&self`. Put transactional state in the
host journal; if interior mutability is needed, release its borrow before a
child call. A borrow held across reentry can panic even with shared dispatch.

## Reproduce the review finding

The following was run with `rustc 1.98.0-nightly (01dfd7924 2026-06-15)` and
the matching Miri component:

```sh
cargo +nightly miri test -p evm2 --no-default-features --features std --lib precompile_can_call_another_precompile
```

Before the fix, Miri reports undefined behavior at
`crates/evm2/src/precompiles/mod.rs:161`, where the nested execution creates its
provider reference. It identifies the protected exclusive borrow created at
`crates/evm2/src/evm/mod.rs:383` as the conflicting reference.

After the fix, that test passes. The following regression also passes Miri:

```sh
cargo +nightly miri test -p evm2 --no-default-features --features std --lib shared_provider_retains_state_across_reentry_and_releases_idle_mutation
```

It records outer entry, inner entry, inner exit, and outer exit in the same
provider, then checks that idle mutable access is available again. This catches
implementations that temporarily remove dispatch or discard provider state to
avoid aliasing.

Miri's Stacked Borrows model is experimental. These results address the
reproduced provider aliasing violation, rather than proving every unsafe path in
the interpreter. Miri also reports existing integer-to-pointer casts in the
type-erasure machinery; those require a separate provenance review.
