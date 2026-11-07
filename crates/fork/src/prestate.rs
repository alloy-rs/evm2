//! Predicts the remote state a block reads, so it can be fetched in a few JSON-RPC batches
//! instead of one blocking round trip per cold read.
//!
//! The exact prestate comes from `debug_traceBlockByNumber` with the `prestateTracer` when the
//! endpoint serves it. Otherwise each transaction's `eth_createAccessList` against the pinned
//! state names the accounts and slots it touches; those keys are then fetched in batches.

use crate::{ForkError, ForkResult, backend::account_from_parts};
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{
    Address, B256, Bytes, U256,
    map::{AddressMap, AddressSet, HashSet},
};
use alloy_provider::{DynProvider, Network, Provider};
use evm2::{evm::AccountInfo, interpreter::Word};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{borrow::Cow, sync::Arc};

/// `eth_createAccessList` calls per JSON-RPC batch; each one executes a transaction.
const ACCESS_LIST_BATCH_SIZE: usize = 50;

/// `eth_createAccessList` batches in flight at once.
const ACCESS_LIST_CONCURRENCY: usize = 8;

/// Remote state a block is predicted to read.
#[derive(Debug)]
pub(crate) enum Prediction {
    /// Exact values from a prestate trace.
    State { accounts: Vec<(Address, Option<AccountInfo>)>, slots: Vec<(Address, Word, Word)> },
    /// Keys from access lists, still to be fetched.
    Keys { accounts: Vec<Address>, slots: Vec<(Address, Word)> },
}

/// Predicts the state `block`'s transactions read from the state at `state_block`.
pub(crate) async fn predict<N: Network>(
    provider: &DynProvider<N>,
    block: BlockId,
    state_block: BlockId,
) -> ForkResult<Prediction> {
    // A prestate trace holds the state before `block`: usable only when the pin is its parent.
    if !pinned_to_parent(provider, block, state_block).await {
        tracing::debug!(target: "evm2::fork", "pinned block is not the parent, predicting with access lists");
        return access_lists(provider, block, state_block).await;
    }
    match trace_prestate(provider, block).await {
        Ok(prediction) => return Ok(prediction),
        Err(err) => {
            tracing::debug!(target: "evm2::fork", %err, "prestate trace unavailable, predicting with access lists");
        }
    }
    access_lists(provider, block, state_block).await
}

/// Whether `state_block` is `block`'s parent. A tag cannot be checked and counts as not.
async fn pinned_to_parent<N: Network>(
    provider: &DynProvider<N>,
    block: BlockId,
    state_block: BlockId,
) -> bool {
    let method = match block {
        BlockId::Hash(_) => "eth_getBlockByHash",
        BlockId::Number(_) => "eth_getBlockByNumber",
    };
    let Ok(header) =
        provider.raw_request::<_, Value>(Cow::Borrowed(method), (block_param(block), false)).await
    else {
        return false;
    };
    match state_block {
        BlockId::Hash(hash) => header
            .get("parentHash")
            .and_then(Value::as_str)
            .and_then(|parent| parent.parse::<B256>().ok())
            .is_some_and(|parent| parent == hash.block_hash),
        BlockId::Number(BlockNumberOrTag::Number(number)) => {
            header.get("number").and_then(parse_u64).is_some_and(|n| n == number + 1)
        }
        BlockId::Number(_) => false,
    }
}

/// Reads the block's prestate trace, keeping each account's and slot's first value: the value
/// before the block.
async fn trace_prestate<N: Network>(
    provider: &DynProvider<N>,
    block: BlockId,
) -> Result<Prediction, alloy_provider::transport::TransportError> {
    let method = match block {
        BlockId::Hash(_) => "debug_traceBlockByHash",
        BlockId::Number(_) => "debug_traceBlockByNumber",
    };
    let traces = provider
        .raw_request::<_, Vec<Value>>(
            Cow::Borrowed(method),
            (block_param(block), json!({ "tracer": "prestateTracer" })),
        )
        .await?;
    let mut accounts = AddressMap::<Option<AccountInfo>>::default();
    let mut slots = Vec::new();
    let mut seen_slots = HashSet::<(Address, Word)>::default();
    for trace in &traces {
        let Some(result) = trace.get("result").and_then(Value::as_object) else { continue };
        for (address, account) in result {
            let Ok(address) = address.parse::<Address>() else { continue };
            accounts.entry(address).or_insert_with(|| {
                let balance = account.get("balance").and_then(parse_u256).unwrap_or_default();
                let nonce = account.get("nonce").and_then(parse_u64).unwrap_or_default();
                let code = account
                    .get("code")
                    .and_then(Value::as_str)
                    .and_then(|code| code.parse::<Bytes>().ok())
                    .unwrap_or_default();
                account_from_parts(balance, nonce, code)
            });
            let Some(storage) = account.get("storage").and_then(Value::as_object) else { continue };
            for (slot, value) in storage {
                if let Ok(slot) = slot.parse::<B256>()
                    && let Some(value) = parse_u256(value)
                    && seen_slots.insert((address, slot.into()))
                {
                    slots.push((address, slot.into(), value));
                }
            }
        }
    }
    Ok(Prediction::State { accounts: accounts.into_iter().collect(), slots })
}

/// Collects the keys every transaction of the block touches, from `eth_createAccessList` run
/// against the state at `state_block`.
async fn access_lists<N: Network>(
    provider: &DynProvider<N>,
    block: BlockId,
    state_block: BlockId,
) -> ForkResult<Prediction> {
    let method = match block {
        BlockId::Hash(_) => "eth_getBlockByHash",
        BlockId::Number(_) => "eth_getBlockByNumber",
    };
    let response = provider
        .raw_request::<_, Value>(Cow::Borrowed(method), (block_param(block), true))
        .await
        .map_err(|err| ForkError::GetFullBlock(block, Arc::new(err)))?;
    if response.is_null() {
        return Err(ForkError::BlockNotFound(block));
    }
    let mut accounts = AddressSet::default();
    if let Some(miner) = response.get("miner").and_then(parse_address) {
        accounts.insert(miner);
    }
    let requests = response
        .get("transactions")
        .and_then(Value::as_array)
        .map(|txs| {
            txs.iter()
                .map(|tx| {
                    accounts.extend(
                        ["from", "to"]
                            .iter()
                            .filter_map(|field| tx.get(*field).and_then(parse_address)),
                    );
                    call_request(tx)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let state_block = block_param(state_block);
    let chunks = requests.chunks(ACCESS_LIST_BATCH_SIZE).map(<[Value]>::to_vec).collect::<Vec<_>>();
    let results = stream::iter(chunks)
        .map(|chunk| create_access_lists(provider, chunk, state_block.clone()))
        .buffer_unordered(ACCESS_LIST_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    let mut slots = HashSet::<(Address, Word)>::default();
    for result in results {
        for list in result.map_err(|err| ForkError::CreateAccessList(Arc::new(err)))? {
            let Some(items) = list.get("accessList").and_then(Value::as_array) else { continue };
            for item in items {
                let Some(address) = item.get("address").and_then(parse_address) else { continue };
                accounts.insert(address);
                for key in item.get("storageKeys").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(key) = key.as_str().and_then(|key| key.parse::<B256>().ok()) {
                        slots.insert((address, key.into()));
                    }
                }
            }
        }
    }
    Ok(Prediction::Keys {
        accounts: accounts.into_iter().collect(),
        slots: slots.into_iter().collect(),
    })
}

/// Sends one batch of `eth_createAccessList` calls. A call that fails, such as a transaction
/// that reverts against the pinned state, only loses its own prediction.
async fn create_access_lists<N: Network>(
    provider: &DynProvider<N>,
    requests: Vec<Value>,
    state_block: Value,
) -> Result<Vec<Value>, alloy_provider::transport::TransportError> {
    let client = provider.client();
    let mut batch = alloy_rpc_client::BatchRequest::new(client);
    let mut waiters = Vec::with_capacity(requests.len());
    for request in requests {
        waiters.push(
            batch.add_call::<_, Value>("eth_createAccessList", &(request, state_block.clone()))?,
        );
    }
    batch.send().await?;
    let mut lists = Vec::with_capacity(waiters.len());
    for waiter in waiters {
        if let Ok(list) = waiter.await {
            lists.push(list);
        }
    }
    Ok(lists)
}

/// Builds an `eth_call`-style request from a transaction, without nonce and fee fields so the
/// call runs against the pinned state whatever ran before it in the block.
fn call_request(tx: &Value) -> Value {
    let mut request = serde_json::Map::new();
    for field in ["from", "to", "input", "value", "gas", "accessList", "authorizationList"] {
        if let Some(value) = tx.get(field).filter(|value| !value.is_null()) {
            request.insert(field.to_string(), value.clone());
        }
    }
    Value::Object(request)
}

fn block_param(block: BlockId) -> Value {
    match block {
        BlockId::Hash(hash) => json!(hash.block_hash),
        BlockId::Number(number) => json!(number),
    }
}

fn parse_address(value: &Value) -> Option<Address> {
    value.as_str()?.parse().ok()
}

fn parse_u256(value: &Value) -> Option<U256> {
    match value {
        Value::String(value) => value.parse().ok(),
        Value::Number(value) => value.as_u64().map(U256::from),
        _ => None,
    }
}

fn parse_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => u64::from_str_radix(value.trim_start_matches("0x"), 16).ok(),
        _ => None,
    }
}
