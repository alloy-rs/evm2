# evm2-superinstr

A superinstruction backend for evm2: a linear-time translator from EVM bytecode to records, and a
threaded executor that runs them. It plugs into `Evm` through the same `InterpreterRunner` hook
as the LLVM JIT, so a node chooses between the interpreter, this backend and the JIT by installing
a runner.

```rust,ignore
use evm2_superinstr::{SuperInstrBackend, SuperInstrConfig, SuperInstrRunner};

let backend = SuperInstrBackend::new(SuperInstrConfig::default());
evm.set_interpreter_runner(SuperInstrRunner::new(backend.clone()));
```

The backend translates a contract the first time a frame runs it and caches the result by code
hash, host type and opcode table. There is no compile thread or hot threshold.

## Translation

One pass per basic block keeps a virtual stack:

- `PUSH`, `DUP`, `SWAP` and `POP` only rename entries, so they cost nothing at run time.
- Arithmetic, compares, bitwise operations, `MLOAD`, `MSTORE`, `CALLDATALOAD` and frame fields
  (`ADDRESS`, `CALLER`, `CALLVALUE`, `CALLDATASIZE`, `RETURNDATASIZE`) become records that read
  immediates or stack slots directly. Operations on constants fold.
- A compare, with any `ISZERO`s after it, fuses into the `JUMPI` that consumes it.
- Code after a `JUMPI` that only the `JUMPI` enters keeps the virtual stack.
- Every other instruction runs the host's own implementation on the canonical stack, so custom
  hosts and opcode tables keep their semantics. Blocks the translator cannot handle hand the frame
  to the interpreter.

Before an exit or a host instruction the virtual stack is written back with a parallel copy, so
blocks start and end with the canonical EVM stack. A second pass over each block writes results
straight to the slot they reach at the next exit, which removes most of those copies.

Static gas and stack bounds are charged once per section, as the JIT does: a block, split after
instructions that read the remaining gas.

## Execution

Each record links to a handler specialized for its kind and operand modes, and handlers pass
control with tail calls (`become`) on nightly compilers, or through a dispatch loop otherwise.

## Testing

```bash
cargo nextest run -p evm2-eest --features superinstr --test eest --ignore-default-filter -E 'test(/superinstr/)'
cargo test -p evm2-superinstr
cargo run -p evm2-superinstr --example translate --profile release   # translation throughput
```

The superinstruction suites run the EEST state and blockchain tests and the benchmark fixtures in
`data/` through the backend.

## Benchmarks

```bash
# Interpreter, LLVM JIT and superinstructions on the transaction fixtures.
cargo bench -p evm2-cli --bench evm --features jit,superinstr
# Mainnet replay on every backend, with warm translations and compiled code.
EVM2_BENCH_REPLAY_BACKENDS=1 EVM2_EEST_SHARED_JIT=1 EVM2_BENCH_SHARD=interpreter \
    cargo bench -p evm2-cli --bench evm --features jit,superinstr -- replay
```
