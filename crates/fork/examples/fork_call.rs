//! Forks a live chain with [`MultiFork`] and runs ERC-20 calls through evm2, checking them
//! against `eth_call`.
//!
//! ```sh
//! ETH_RPC_URL=https://... cargo run -p evm2-fork --example fork_call
//! ```
//!
//! The fork's state is cached under the system temp directory, so a second run at the same block
//! makes no state requests.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256, address};
use alloy_provider::{Provider, network::Ethereum};
use alloy_rpc_types_eth::TransactionRequest;
use alloy_sol_types::{SolCall, sol};
use evm2::{
    BaseEvmTypes, Evm, Precompiles,
    ethereum::{TxEnvelope, ethereum_tx_registry},
    evm::DbStats,
};
use evm2_fork::{CreateFork, ForkedDatabase, MultiFork, http_provider};
use std::time::Instant;

sol! {
    function totalSupply() external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256);
}

const USDC: Address = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const HOLDER: Address = address!("0x37305B1cD40574E4C5Ce33f8e8306Be057fD7341");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rpc_url = std::env::var("ETH_RPC_URL")?;
    let block = std::env::var("FORK_BLOCK").ok().map(|block| block.parse()).transpose()?;
    let forks = MultiFork::<Ethereum>::spawn();
    let fork = forks.create_fork(
        CreateFork::new(&rpc_url)
            .with_block(block)
            .with_caching(true)
            .with_cache_root(Some(std::env::temp_dir().join("evm2-fork-example")))
            .with_prewarm_bal(true),
    )?;
    let env = fork.env;
    println!(
        "forked chain {} at block {} ({:?}), id {}, {} accounts cached",
        env.chain_id,
        env.block_number,
        env.spec_id,
        fork.id,
        fork.backend.accounts_len()
    );

    // Like `eth_call`, run with a zero base fee so a zero gas price is accepted.
    let mut block_env = env.block;
    block_env.basefee = U256::ZERO;
    let mut evm = Evm::<'static, BaseEvmTypes>::new(
        env.spec_id,
        block_env,
        ethereum_tx_registry(env.spec_id),
        DbStats::new(ForkedDatabase::new(fork.backend)),
        Precompiles::base(env.spec_id),
    );

    let provider = http_provider::<Ethereum>(&rpc_url)?;
    let runtime = tokio::runtime::Runtime::new()?;
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
        let expected = runtime
            .block_on(async { provider.call(request).block(env.block_number.into()).await })?;
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
    let stats = evm.database_as::<DbStats<ForkedDatabase>>().expect("fork database").counts();
    println!("database reads: {stats:?}");
    Ok(())
}
