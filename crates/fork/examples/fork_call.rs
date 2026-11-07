//! Forks a live chain and runs ERC-20 calls through evm2, checking them against `eth_call`.
//!
//! ```sh
//! ETH_RPC_URL=https://... cargo run -p evm2-fork --example fork_call
//! ```

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256, address};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::{BlockId, TransactionRequest};
use alloy_sol_types::{SolCall, sol};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::{DbRef, DbStats},
};
use evm2_fork::{BlockchainDb, BlockchainDbMeta, ForkDB, SharedBackend};
use std::time::Instant;

sol! {
    function totalSupply() external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256);
}

const USDC: Address = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const HOLDER: Address = address!("0x37305B1cD40574E4C5Ce33f8e8306Be057fD7341");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rpc_url = std::env::var("ETH_RPC_URL")?.parse()?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let runtime = tokio::runtime::Runtime::new()?;
    let (chain_id, header) = runtime.block_on(async {
        let chain_id = provider.get_chain_id().await?;
        let block = provider.get_block(BlockId::latest()).await?.expect("latest block");
        Ok::<_, Box<dyn std::error::Error>>((chain_id, block.header))
    })?;
    let number = header.number;
    println!("forking chain {chain_id} at block {number}");

    let db = BlockchainDb::new(BlockchainDbMeta::new(chain_id, number), None);
    let backend = SharedBackend::spawn_backend_thread(provider.clone(), db, Some(number.into()));
    let block = BlockEnvExt {
        number: U256::from(number),
        timestamp: U256::from(header.timestamp),
        gas_limit: U256::from(header.gas_limit),
        beneficiary: header.beneficiary,
        prevrandao: header.mix_hash.into(),
        ..Default::default()
    };
    let mut evm = Evm::<'static, BaseEvmTypes>::new(
        SpecId::OSAKA,
        block,
        ethereum_tx_registry(SpecId::OSAKA),
        DbStats::new(ForkDB::new(DbRef::new(backend))),
        Precompiles::base(SpecId::OSAKA),
    );

    let calls: [(&str, Bytes); 2] = [
        ("totalSupply()", totalSupplyCall {}.abi_encode().into()),
        ("balanceOf(holder)", balanceOfCall { owner: HOLDER }.abi_encode().into()),
    ];
    for (name, input) in calls {
        let start = Instant::now();
        let tx = TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(USDC),
            gas_limit: 1_000_000,
            input: input.clone(),
            ..Default::default()
        });
        let result = evm.transact(&Recovered::new_unchecked(tx, Address::ZERO))?.discard();
        let elapsed = start.elapsed();
        let request = TransactionRequest::default().to(USDC).input(input.into());
        let expected =
            runtime.block_on(async { provider.call(request).block(number.into()).await })?;
        println!(
            "{name}: status={} gas={} evm2={} eth_call={} ({elapsed:?})",
            result.status,
            result.tx_gas_used(),
            U256::from_be_slice(&result.output),
            U256::from_be_slice(&expected),
        );
        assert!(result.status);
        assert_eq!(result.output, expected);
    }
    let stats = evm.database_as::<DbStats<ForkDB>>().expect("fork database").counts();
    println!("database reads: {stats:?}");
    Ok(())
}
