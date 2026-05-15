//! Transaction reproduction test infrastructure.
//!
//! This module provides reusable tooling for replaying transactions with prestate data
//! captured from mainnet using the prestate tracer.
//!
//! The prestate JSON fixture should be the raw RPC response from `debug_traceCall` or
//! `debug_traceTransaction` with the prestate tracer. Transaction data is provided
//! separately as a constructed `TxEnv`.
//!
//! # Example
//!
//! ```ignore
//! use crate::repro::ReproContext;
//!
//! // Raw prestate tracer RPC response (copy-paste from RPC)
//! const PRESTATE: &str = include_str!("../../../testdata/repro/my-prestate.json");
//!
//! #[test]
//! fn test_my_trace() {
//!     let ctx = ReproContext::from_prestate_response(PRESTATE)
//!         .with_block_number(19660754); // or .with_spec_id(SpecId::CANCUN)
//!
//!     // Construct TxEnv from transaction data
//!     let tx_env = TxEnv {
//!         caller: address!("..."),
//!         kind: TransactTo::Call(address!("...")),
//!         data: hex!("...").into(),
//!         nonce: 123,
//!         gas_limit: 150000,
//!         ..Default::default()
//!     };
//!
//!     let mut inspector = TracingInspector::new(
//!         TracingInspectorConfig::from_geth_prestate_config(&PreStateConfig::default())
//!     );
//!
//!     let mut evm = Context::mainnet()
//!         .with_db(ctx.db.clone())
//!         .modify_cfg_chained(|cfg| cfg.spec = ctx.spec_id)
//!         .build_mainnet()
//!         .with_inspector(&mut inspector);
//!
//!     let res = evm.inspect_tx(tx_env).unwrap();
//!     // ... assertions on trace results
//! }
//! ```

mod prestate;

use crate::utils::{AccountInfo, Bytecode, CacheDB, EmptyDB, SpecId};
use alloy_hardforks::EthereumHardfork;
use alloy_primitives::Address;
use alloy_rpc_types_trace::geth::AccountState;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Convert an Ethereum hardfork to an evm2 SpecId.
pub const fn spec_id_from_ethereum_hardfork(hardfork: EthereumHardfork) -> SpecId {
    match hardfork {
        EthereumHardfork::Frontier => SpecId::FRONTIER,
        EthereumHardfork::Homestead | EthereumHardfork::Dao => SpecId::HOMESTEAD,
        EthereumHardfork::Tangerine => SpecId::TANGERINE,
        EthereumHardfork::SpuriousDragon => SpecId::SPURIOUS_DRAGON,
        EthereumHardfork::Byzantium => SpecId::BYZANTIUM,
        EthereumHardfork::Constantinople | EthereumHardfork::Petersburg => SpecId::PETERSBURG,
        EthereumHardfork::Istanbul | EthereumHardfork::MuirGlacier => SpecId::ISTANBUL,
        EthereumHardfork::Berlin => SpecId::BERLIN,
        EthereumHardfork::London
        | EthereumHardfork::ArrowGlacier
        | EthereumHardfork::GrayGlacier => SpecId::LONDON,
        EthereumHardfork::Paris => SpecId::MERGE,
        EthereumHardfork::Shanghai => SpecId::SHANGHAI,
        EthereumHardfork::Cancun => SpecId::CANCUN,
        EthereumHardfork::Prague => SpecId::PRAGUE,
        EthereumHardfork::Osaka
        | EthereumHardfork::Bpo1
        | EthereumHardfork::Bpo2
        | EthereumHardfork::Bpo3
        | EthereumHardfork::Bpo4
        | EthereumHardfork::Bpo5 => SpecId::OSAKA,
        EthereumHardfork::Amsterdam => SpecId::AMSTERDAM,
        _ => SpecId::NEXT,
    }
}

/// Determine the SpecId from a mainnet block number.
pub const fn spec_id_from_block(block_number: u64) -> SpecId {
    spec_id_from_ethereum_hardfork(EthereumHardfork::from_mainnet_block_number(block_number))
}

/// Build a CacheDB from prestate AccountState map.
pub fn build_db_from_prestate(prestate: &BTreeMap<Address, AccountState>) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());

    for (addr, state) in prestate {
        let balance = state.balance.unwrap_or_default();
        let nonce = state.nonce.unwrap_or_default();
        let code = state.code.as_ref().map(|c| Bytecode::new_raw(c.clone()));

        db.insert_account_info(
            addr,
            AccountInfo {
                balance,
                nonce,
                code_hash: code.as_ref().map(|c| c.hash_slow()).unwrap_or_default(),
                code,
                ..Default::default()
            },
        );

        // Insert storage
        for (slot, value) in &state.storage {
            db.insert_account_storage(addr, &(*slot).into(), &(*value).into());
        }
    }

    db
}

/// Wrapper for parsing raw prestate tracer RPC response.
///
/// Handles both direct prestate maps and JSON-RPC wrapped responses.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PrestateResponse {
    /// Direct prestate map (e.g., from `result` field)
    Direct(BTreeMap<Address, AccountState>),
    /// JSON-RPC wrapped response
    Wrapped { result: BTreeMap<Address, AccountState> },
}

impl PrestateResponse {
    fn into_prestate(self) -> BTreeMap<Address, AccountState> {
        match self {
            Self::Direct(prestate) => prestate,
            Self::Wrapped { result } => result,
        }
    }
}

/// Context for replaying a transaction with prestate data.
///
/// The prestate is loaded from a JSON fixture (raw RPC response format).
/// Transaction data is provided separately via RLP bytes or constructed `TxEnv`.
#[derive(Debug, Clone)]
pub struct ReproContext {
    /// The prestate accounts loaded from the fixture.
    pub prestate: BTreeMap<Address, AccountState>,
    /// The EVM spec to use for execution.
    pub spec_id: SpecId,
    /// The database populated with prestate.
    pub db: CacheDB<EmptyDB>,
}

impl ReproContext {
    /// Create a ReproContext from a raw prestate tracer RPC response.
    ///
    /// Accepts both the raw `result` field content or the full JSON-RPC response.
    ///
    /// # Example
    /// ```ignore
    /// // Direct prestate map
    /// let ctx = ReproContext::from_prestate_response(r#"{"0x1234...": {"balance": "0x0"}}"#);
    ///
    /// // Or full JSON-RPC response
    /// let ctx = ReproContext::from_prestate_response(r#"{"jsonrpc":"2.0","id":1,"result":{...}}"#);
    /// ```
    pub fn from_prestate_response(json: &str) -> Self {
        let response: PrestateResponse = serde_json::from_str(json).expect("valid prestate JSON");
        let prestate = response.into_prestate();
        let db = build_db_from_prestate(&prestate);

        Self { prestate, spec_id: SpecId::PRAGUE, db }
    }

    /// Set the spec ID (hardfork) for EVM execution.
    #[must_use]
    pub const fn with_spec_id(mut self, spec_id: SpecId) -> Self {
        self.spec_id = spec_id;
        self
    }

    /// Set the spec ID based on a mainnet block number.
    #[must_use]
    pub const fn with_block_number(mut self, block_number: u64) -> Self {
        self.spec_id = spec_id_from_block(block_number);
        self
    }
}
