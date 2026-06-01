# alloy-rs/evm Port Progress

Source repository: `/Users/doni/github/alloy-rs/evm`

Source commit: `4c082e2541323d6ce34d3e0ba8b0ba574f0c85f1`

## Done

- Changed `PrecompileProvider` so execution receives `&mut Evm<T>`.
- Updated the built-in `Precompiles` provider to the new provider API.
- Added coverage that verifies a precompile can observe the host EVM during execution.
- Added `crates/evm2/src/evm/block.rs` for block-level APIs as inherent `Evm` methods.
- Added block environment accessors: `block`, `block_mut`, and `set_block`.
- Added pre-block system call helpers for EIP-2935 and EIP-4788.
- Added post-block system call helpers for EIP-7002 and EIP-7251 with EIP-7685 request collection.
- Added individual block system-call methods matching alloy's `SystemCaller` surface.
- Added an Ethereum block transaction loop that enforces capped remaining block gas before each transaction.
- Added block gas counters for cumulative transaction gas, regular gas, state gas, and final block gas.
- Added post-block balance increment methods for block rewards, ommer rewards, and withdrawals.
- Added DAO hardfork constants and method-based balance drain helpers, including the mainnet DAO fork block gate.
- Added `EthBlockExecutionCtx` and high-level `Evm::execute_block` orchestration for pre-block system calls, transactions, post-block system calls, post-block balance increments, requests, and optional mainnet DAO handling.

## In Progress

- Compare the new block methods against alloy-rs/evm `EthBlockExecutor` behavior and close semantic gaps.
- Decide whether EIP-6110 deposit request parsing belongs in evm2 block methods or caller-specific receipt code.
- Port any missing revm/alloy helper types only when they are needed by the method-based evm2 API.

## Not Planned

- Block access list support.
