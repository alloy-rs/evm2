# evm2-replayer

Replays flattened `revm-oomph` mainnet block corpora with `evm2`.

The replayer decodes each corpus block, reconstructs the consensus block from
the raw RLP header/body, prepares an `evm2` block environment from the header,
executes every transaction, and checks that the sum of transaction gas matches
the header `gas_used`.

It is a transaction execution and gas replay tool. It does not yet validate the
final state root, receipts root, logs bloom, or EIP-7685 request outputs.

## Build

```sh
cargo build --release -p evm2-replayer
```

## Usage

```text
Usage: evm2-replayer [OPTIONS] <PATH>

Arguments:
  <PATH>  Replay corpus directory, blocks directory, or single block file

Options:
      --preload  Prepare all blocks before executing them
  -h, --help     Print help
  -V, --version  Print version
```

Run against a generated corpus directory:

```sh
./target/release/evm2-replayer corpus/mainnet-first-50-24855016-24855065
```

The input path can be any of:

- a generated corpus directory containing `manifest.json`,
- a directory containing replay `.bin` files,
- a directory containing a `blocks/` subdirectory with replay `.bin` files,
- a single replay `.bin` file.

Directory discovery is shallow. Manifest input uses manifest order; ad-hoc
directory input uses lexicographic filename order.

## Preload Mode

By default, each block is read, prepared, executed, and verified before moving
to the next block.

Use `--preload` to read and prepare every block before timing execution:

```sh
./target/release/evm2-replayer --preload corpus/mainnet-first-50-24855016-24855065
```

Preload mode is useful when comparing EVM execution throughput because artifact
decoding, transaction validation, state loading, and block environment
construction are kept out of the reported execution pass.

## Output

The replayer prints progress on stderr and a final gas summary on stdout:

```text
executed 1000 blocks (450432 txs) from corpus/mainnet-last-1000-24855016-24856015 in 23.34s (evm2-only)
evm2-only gas used: 30133639516 (30133.64 Mgas) at 1290.89 Mgas/s
```
