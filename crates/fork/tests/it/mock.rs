//! An in-process JSON-RPC node serving fixed per-block state.

use alloy_consensus::{Signed, TxEnvelope, TxLegacy, transaction::Recovered};
use alloy_eips::eip7928::BlockAccessList;
use alloy_json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy_primitives::{Address, B256, Bytes, Signature, U256, keccak256, map::HashMap};
use alloy_provider::{RootProvider, transport::TransportError};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{Block, BlockTransactions, Header, Transaction};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll},
    time::Duration,
};
use tower::Service;

#[derive(Clone, Debug, Default)]
pub(crate) struct MockAccount {
    pub(crate) balance: U256,
    pub(crate) nonce: u64,
    pub(crate) code: Bytes,
    pub(crate) storage: HashMap<U256, U256>,
}

#[derive(Debug, Default)]
struct MockState {
    chain_id: u64,
    /// Accounts set at a block apply from that block on.
    accounts: BTreeMap<u64, HashMap<Address, MockAccount>>,
    /// Block hashes by number; the highest is `latest`.
    blocks: BTreeMap<u64, B256>,
    /// Block access lists by block hash.
    bals: HashMap<B256, BlockAccessList>,
    transactions: HashMap<B256, Transaction>,
    supports_account_info: bool,
    /// Calls per method.
    calls: HashMap<String, usize>,
    /// Resolved block number of every state call, in arrival order.
    state_blocks: Vec<u64>,
}

/// A JSON-RPC transport answering from in-memory state, optionally after a delay.
#[derive(Clone, Debug)]
pub(crate) struct MockNode {
    state: Arc<Mutex<MockState>>,
    delay: Duration,
}

impl Default for MockNode {
    fn default() -> Self {
        let state = MockState { chain_id: 1, ..Default::default() };
        Self { state: Arc::new(Mutex::new(state)), delay: Duration::ZERO }
    }
}

impl MockNode {
    pub(crate) fn with_delay(delay: Duration) -> Self {
        Self { delay, ..Default::default() }
    }

    /// Hash of mock block `number`.
    pub(crate) fn block_hash_of(number: u64) -> B256 {
        keccak256(number.to_be_bytes())
    }

    pub(crate) fn set_chain_id(&self, chain_id: u64) {
        self.lock().chain_id = chain_id;
    }

    /// Adds blocks `0..=latest` with hashes from [`Self::block_hash_of`].
    pub(crate) fn add_blocks(&self, latest: u64) {
        let mut state = self.lock();
        for number in 0..=latest {
            state.blocks.insert(number, Self::block_hash_of(number));
        }
    }

    /// Sets an account from `block` on.
    pub(crate) fn insert_account_at(&self, block: u64, address: Address, account: MockAccount) {
        self.lock().accounts.entry(block).or_default().insert(address, account);
    }

    /// Sets an account from genesis on.
    pub(crate) fn insert_account(&self, address: Address, account: MockAccount) {
        self.insert_account_at(0, address, account);
    }

    pub(crate) fn insert_bal(&self, block: u64, bal: BlockAccessList) {
        self.lock().bals.insert(Self::block_hash_of(block), bal);
    }

    /// Stores a legacy transaction from `from` and returns its hash.
    pub(crate) fn insert_transaction(&self, from: Address, nonce: u64) -> B256 {
        let tx = TxLegacy { nonce, ..Default::default() };
        let hash = keccak256(nonce.to_be_bytes());
        let signed = Signed::new_unchecked(tx, Signature::test_signature(), hash);
        let tx = Transaction {
            inner: Recovered::new_unchecked(TxEnvelope::Legacy(signed), from),
            block_hash: None,
            block_number: None,
            transaction_index: None,
            effective_gas_price: None,
            block_timestamp: None,
        };
        self.lock().transactions.insert(hash, tx);
        hash
    }

    pub(crate) fn set_supports_account_info(&self, supported: bool) {
        self.lock().supports_account_info = supported;
    }

    pub(crate) fn calls(&self, method: &str) -> usize {
        self.lock().calls.get(method).copied().unwrap_or_default()
    }

    pub(crate) fn total_calls(&self) -> usize {
        self.lock().calls.values().sum()
    }

    /// Calls that read account or storage state.
    pub(crate) fn state_calls(&self) -> usize {
        [
            "eth_getBalance",
            "eth_getTransactionCount",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_getAccountInfo",
        ]
        .iter()
        .map(|method| self.calls(method))
        .sum()
    }

    /// The block number every state call resolved to.
    pub(crate) fn state_blocks(&self) -> Vec<u64> {
        self.lock().state_blocks.clone()
    }

    pub(crate) fn provider(&self) -> RootProvider {
        RootProvider::new(RpcClient::new(self.clone(), true))
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn answer(&self, request: &SerializedRequest) -> Response {
        let params = request
            .params()
            .map(|params| Vec::<Value>::deserialize(params).unwrap())
            .unwrap_or_default();
        let mut state = self.lock();
        *state.calls.entry(request.method().to_string()).or_default() += 1;
        let unknown_block = || ErrorPayload {
            code: -32000,
            message: Cow::Borrowed("header not found"),
            data: None,
        };
        let result = match request.method() {
            "eth_chainId" => Ok(json!(U256::from(state.chain_id))),
            "eth_getBalance" => {
                state.account(&params, 1).map(|account| json!(account.balance)).ok_or_else(unknown_block)
            }
            "eth_getTransactionCount" => state
                .account(&params, 1)
                .map(|account| json!(U256::from(account.nonce)))
                .ok_or_else(unknown_block),
            "eth_getCode" => {
                state.account(&params, 1).map(|account| json!(account.code)).ok_or_else(unknown_block)
            }
            "eth_getAccountInfo" if state.supports_account_info => state
                .account(&params, 1)
                .map(|account| {
                    json!({ "balance": account.balance, "nonce": U256::from(account.nonce), "code": account.code })
                })
                .ok_or_else(unknown_block),
            "eth_getStorageAt" => {
                let slot = params[1].as_str().unwrap().parse::<U256>().unwrap();
                state
                    .account(&params, 2)
                    .map(|account| {
                        json!(B256::from(account.storage.get(&slot).copied().unwrap_or_default()))
                    })
                    .ok_or_else(unknown_block)
            }
            "eth_getBlockByNumber" | "eth_getBlockByHash" => {
                Ok(state.resolve(&params[0]).map_or(Value::Null, |number| {
                    let header = Header {
                        hash: Self::block_hash_of(number),
                        inner: alloy_consensus::Header {
                            number,
                            timestamp: 1_700_000_000 + number * 12,
                            gas_limit: 30_000_000,
                            base_fee_per_gas: Some(7),
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    json!(Block::<Transaction>::empty(header)
                        .with_transactions(BlockTransactions::Full(Vec::new())))
                }))
            }
            "eth_getTransactionByHash" => {
                let hash = params[0].as_str().unwrap().parse::<B256>().unwrap();
                Ok(state.transactions.get(&hash).map_or(Value::Null, |tx| json!(tx)))
            }
            "eth_getBlockAccessList" => {
                let number = state.resolve(&params[0]);
                Ok(number
                    .and_then(|number| state.bals.get(&Self::block_hash_of(number)))
                    .map_or(Value::Null, |bal| json!(bal)))
            }
            _ => Err(ErrorPayload {
                code: -32601,
                message: Cow::Owned(format!("the method {} does not exist", request.method())),
                data: None,
            }),
        };
        let payload = match result {
            Ok(result) => {
                ResponsePayload::Success(RawValue::from_string(result.to_string()).unwrap())
            }
            Err(err) => ResponsePayload::Failure(err),
        };
        Response { id: request.id().clone(), payload }
    }
}

impl MockState {
    /// Resolves a block tag, number, hash or EIP-1898 block object to a known block number.
    fn resolve(&self, tag: &Value) -> Option<u64> {
        let latest = self.blocks.keys().next_back().copied().unwrap_or_default();
        let number = match tag {
            Value::String(tag)
                if tag == "latest" || tag == "pending" || tag == "safe" || tag == "finalized" =>
            {
                latest
            }
            Value::String(tag) if tag == "earliest" => 0,
            Value::String(tag) if tag.len() == 66 => {
                let hash = tag.parse::<B256>().unwrap();
                *self.blocks.iter().find(|(_, h)| **h == hash)?.0
            }
            Value::String(tag) => u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap(),
            Value::Object(object) => {
                if let Some(hash) = object.get("blockHash") {
                    return self.resolve(hash);
                }
                return self.resolve(object.get("blockNumber")?);
            }
            _ => panic!("unexpected block tag {tag}"),
        };
        self.blocks.contains_key(&number).then_some(number)
    }

    /// Returns the account named by `params[0]` at the block in `params[block_index]`, or `None`
    /// for an unknown block.
    fn account(&mut self, params: &[Value], block_index: usize) -> Option<MockAccount> {
        let address = params[0].as_str().unwrap().parse::<Address>().unwrap();
        let number = self.resolve(&params[block_index])?;
        self.state_blocks.push(number);
        Some(
            self.accounts
                .range(..=number)
                .rev()
                .find_map(|(_, accounts)| accounts.get(&address).cloned())
                .unwrap_or_default(),
        )
    }
}

impl Service<RequestPacket> for MockNode {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = alloy_transport::TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let node = self.clone();
        Box::pin(async move {
            if !node.delay.is_zero() {
                tokio::time::sleep(node.delay).await;
            }
            Ok(match request {
                RequestPacket::Single(request) => ResponsePacket::Single(node.answer(&request)),
                RequestPacket::Batch(requests) => ResponsePacket::Batch(
                    requests.iter().map(|request| node.answer(request)).collect(),
                ),
            })
        })
    }
}
