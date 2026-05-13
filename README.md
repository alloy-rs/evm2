# evm2

Fast, customizable EVM implementation in Rust.

evm2 keeps Ethereum execution small and direct while making forks, custom transactions, opcodes,
environment extensions, and inspectors easy to wire in.

## Highlights

- up to **2x faster than revm**: a leaner interpreter, compile-time tables, compact instruction
  definitions, and internals abstracted away at zero cost.
- **Simpler customization**: the extensibility of revm, with a smaller surface for custom specs,
  transactions, opcodes, precompiles, environment data, and inspectors.
- **Mainnet behavior**: Ethereum gas accounting, host interaction shape, precompiles, and EEST
  coverage stay aligned with upstream semantics.

## Example

Custom EVMs are just typed extension points plus normal transaction execution:

```rust,ignore
enum CustomSpecId {
    Custom,
    // ...
}

struct CustomTypes;

impl EvmTypes for CustomTypes {
    // ...
}

#[instruction(EvmTypes = CustomTypes)]
fn l1_blocknumber(cx: _) -> out {
    *out = Word::from(cx.state.host().block_env().ext.l1_block_number);
}

fn main() -> Result<()> {
    let spec_id = CustomSpecId::Custom;
    let mut evm = custom_evm(spec_id);
    let tx = custom_tx(&[
        opcode::L1_BLOCKNUMBER,
        // ...
    ]);
    let result = evm.transact(&tx)?;
    // ...
    Ok(())
}
```

See [`crates/evm2/examples/custom_evm`](crates/evm2/examples/custom_evm) for the complete version.

## Benchmarks

```sh
cargo bench -p evm2 --bench evm
EVM2_BENCH_REVM=1 cargo bench -p evm2 --bench evm
```

## Development

```sh
cargo fmt --all
cargo cl
cargo nextest run
```
