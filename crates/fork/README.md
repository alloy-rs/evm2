# evm2-fork

An RPC forking database for evm2, at feature parity with Foundry's forking stack
([foundry-fork-db](https://github.com/foundry-rs/foundry-fork-db) and Foundry's `evm/core/src/fork`).

- [`SharedBackend`] is a cheap, cloneable handle that implements evm2's `DatabaseRef`. Reads are
  served from a shared [`BlockchainDb`] cache and fall back to a background [`BackendHandler`]
  that fetches missing state over JSON-RPC at a pinned block (number or exact hash). Concurrent
  reads of the same account, slot or block hash share one RPC call. Accounts are fetched with
  `eth_getAccountInfo` when the endpoint supports it. The handle also fetches full blocks and
  transactions, runs arbitrary async requests, and accepts bulk cache updates.
- [`BlockchainDb`] persists to a JSON file keyed by chain, block and block hash, merges the hosts
  it was read from, and can take committed state changes.
- [`ForkedDatabase`] is a local overlay with state snapshots (snapshot, revert keeping or
  removing it) and reset to another block; [`ForkDB`] is the plain overlay.
- [`MultiFork`] manages many forks on one background thread: create, share identical forks, roll
  to another block, disk caching under Foundry's `<root>/<chain>/<block>` layout, prefetching a
  new block's cache from the previous block's keys, prewarming from a block access list (EIP-7928),
  and periodic cache flushes.
- [`env::fork_env`] derives the chain ID, spec and block environment of the fork block.

```rust,ignore
let forks = MultiFork::<Ethereum>::spawn();
let fork = forks.create_fork(CreateFork::new(rpc_url).with_caching(true))?;
let db = ForkedDatabase::new(fork.backend);
let evm = Evm::new(fork.env.spec_id, fork.env.block, registry, db, precompiles);
```
