//! An in-process JSON-RPC node serving a fixed state.

use alloy_json_rpc::{RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest};
use alloy_primitives::{Address, B256, Bytes, U256, map::HashMap};
use alloy_provider::{RootProvider, transport::TransportError};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{Block, Header};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::{
    sync::{Arc, Mutex, PoisonError},
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
    accounts: HashMap<Address, MockAccount>,
    block_hashes: HashMap<u64, B256>,
    /// Calls per method.
    calls: HashMap<String, usize>,
    /// Block tag of every state call, in arrival order.
    block_tags: Vec<String>,
}

/// A JSON-RPC transport answering from in-memory state, optionally after a delay.
#[derive(Clone, Debug, Default)]
pub(crate) struct MockNode {
    state: Arc<Mutex<MockState>>,
    delay: Duration,
}

impl MockNode {
    pub(crate) fn with_delay(delay: Duration) -> Self {
        Self { delay, ..Default::default() }
    }

    pub(crate) fn insert_account(&self, address: Address, account: MockAccount) {
        self.lock().accounts.insert(address, account);
    }

    pub(crate) fn insert_block_hash(&self, number: u64, hash: B256) {
        self.lock().block_hashes.insert(number, hash);
    }

    pub(crate) fn calls(&self, method: &str) -> usize {
        self.lock().calls.get(method).copied().unwrap_or_default()
    }

    pub(crate) fn total_calls(&self) -> usize {
        self.lock().calls.values().sum()
    }

    pub(crate) fn block_tags(&self) -> Vec<String> {
        self.lock().block_tags.clone()
    }

    pub(crate) fn provider(&self) -> RootProvider {
        RootProvider::new(RpcClient::new(self.clone(), true))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn answer(&self, request: &SerializedRequest) -> Response {
        let params = request
            .params()
            .map(|params| Vec::<Value>::deserialize(params).unwrap())
            .unwrap_or_default();
        let mut state = self.lock();
        *state.calls.entry(request.method().to_string()).or_default() += 1;
        let address = |index: usize| params[index].as_str().unwrap().parse::<Address>().unwrap();
        let account =
            |state: &MockState, address| state.accounts.get(&address).cloned().unwrap_or_default();
        let result = match request.method() {
            "eth_getBalance" => {
                state.block_tags.push(params[1].as_str().unwrap().to_string());
                json!(account(&state, address(0)).balance)
            }
            "eth_getTransactionCount" => {
                state.block_tags.push(params[1].as_str().unwrap().to_string());
                json!(U256::from(account(&state, address(0)).nonce))
            }
            "eth_getCode" => {
                state.block_tags.push(params[1].as_str().unwrap().to_string());
                json!(account(&state, address(0)).code)
            }
            "eth_getStorageAt" => {
                state.block_tags.push(params[2].as_str().unwrap().to_string());
                let slot = params[1].as_str().unwrap().parse::<U256>().unwrap();
                let value = account(&state, address(0)).storage.get(&slot).copied();
                json!(B256::from(value.unwrap_or_default()))
            }
            "eth_getBlockByNumber" => {
                let number =
                    u64::from_str_radix(params[0].as_str().unwrap().trim_start_matches("0x"), 16)
                        .unwrap();
                match state.block_hashes.get(&number) {
                    Some(&hash) => {
                        let header = Header {
                            hash,
                            inner: alloy_consensus::Header { number, ..Default::default() },
                            ..Default::default()
                        };
                        json!(Block::<B256>::empty(header))
                    }
                    None => Value::Null,
                }
            }
            method => panic!("unexpected RPC method {method}"),
        };
        Response {
            id: request.id().clone(),
            payload: ResponsePayload::Success(RawValue::from_string(result.to_string()).unwrap()),
        }
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
