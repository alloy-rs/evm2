# Mainnet contract corpus

Deployed runtime bytecode of Ethereum mainnet contracts, as returned by
`eth_getCode`, one hex string per file. `scripts/jit/bench.py` compiles every
file in this directory alongside the built-in benchmarks, so CI tracks their
codegen, compile cost and unresolved jumps. They have no calldata or state, so
the runtime analysis skips them.

`slow/` holds contracts that take too long to compile for every CI run. Pass
`--extra-dir data/mainnet --extra-dir data/mainnet/slow` to include them.

Like the other fixtures in `data/`, these are public on-chain bytecodes and
carry no license of their own.

| File | Contract | Address | Why |
| --- | --- | --- | --- |
| `aggregation_router_v6.hex` | 1inch AggregationRouterV6 | `0x111111125421ca6dc452d289314280a0f8842a65` | hot |
| `balancer_vault.hex` | Balancer V2 Vault | `0xba12222222228d8ba445958a75a0704d566bf2c8` | unresolved jumps |
| `curve_3pool.hex` | Curve 3pool (Vyper) | `0xbebc44782c7db0a1a60cb6fe97d0b483032ff1c7` | slow compile |
| `curve_tricrypto2.hex` | Curve tricrypto2 (Vyper) | `0xd51a44d3fae010294c616388b506acda1bfaae46` | slow compile |
| `entry_point_v0_7.hex` | ERC-4337 EntryPoint v0.7 | `0x0000000071727de22e5e9d8baf0edac6f37da032` | hot |
| `seaport_1_6.hex` | Seaport 1.6 | `0x0000000000000068f116a894984e2db1123eb395` | hot |
| `succinct_staking.hex` | SuccinctStaking | `0x7489059640b4aba8ae97bb58fc82889ec8c7446e` | unresolved jumps |
| `uniswap_v3_pool.hex` | Uniswap V3 USDC/WETH 0.05% pool | `0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640` | slow compile, unresolved jumps |
| `uniswap_v4_pool_manager.hex` | Uniswap V4 PoolManager | `0x000000000004444c5dc75cb358380d2e3de08a90` | hot |
| `universal_router.hex` | Uniswap UniversalRouter (V4) | `0x66a9893cc07d91d95644aedd05d03f95e1dba8af` | hot |
| `usdt.hex` | Tether USD (TetherToken) | `0xdac17f958d2ee523a2206206994597c13d831ec7` | hot |
| `slow/operation_impl.hex` | OperationImpl | `0x56e7d4520abfecf10b38368b00723d9bd3c21ee1` | slowest compile, unresolved jumps |
