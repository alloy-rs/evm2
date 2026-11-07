# evm2-fork

An RPC forking database for evm2, modeled on [foundry-fork-db](https://github.com/foundry-rs/foundry-fork-db).

- [`SharedBackend`] is a cheap, cloneable handle that implements evm2's `DatabaseRef`. Reads are
  served from a shared [`BlockchainDb`] cache and fall back to a background [`BackendHandler`]
  that fetches missing state over JSON-RPC at a pinned block.
- The handler deduplicates in-flight requests: concurrent reads of the same account, slot or
  block hash from any number of handles share one RPC call.
- [`BlockchainDb`] can be persisted to a JSON file and reloaded later for the same chain and
  block, so repeated runs make no RPC calls.

Wrap a handle in a [`ForkDB`] (an evm2 `CacheDB`) so execution writes stay local:

```rust,ignore
let provider = ProviderBuilder::new().connect_http(rpc_url);
let meta = BlockchainDbMeta::new(chain_id, block_number);
let backend = SharedBackend::spawn_backend_thread(provider, BlockchainDb::new(meta, None), Some(block_number.into()));
let evm = Evm::new(spec, block_env, registry, ForkDB::new(DbRef::new(backend)), precompiles);
```
