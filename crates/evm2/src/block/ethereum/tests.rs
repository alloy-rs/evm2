use super::*;
use crate::{
    BaseEvmTypes, Precompiles, SpecId,
    block::BlockExecutionError,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::ethereum_tx_registry,
    evm::{
        AccountInfo, BEACON_ROOTS_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS,
        InMemoryDB, WITHDRAWAL_REQUEST_ADDRESS,
    },
};
use alloy_consensus::{Signed, TxLegacy, constants::ETH_TO_WEI, transaction::Recovered};
use alloy_eips::{
    eip2935::HISTORY_STORAGE_CODE, eip4788::BEACON_ROOTS_CODE,
    eip7002::WITHDRAWAL_REQUEST_PREDEPLOY_CODE, eip7251::CONSOLIDATION_REQUEST_PREDEPLOY_CODE,
};
use alloy_primitives::{Signature, TxKind, U256};

const SENDER: Address = Address::repeat_byte(0x01);
const RECIPIENT: Address = Address::repeat_byte(0x02);
const BENEFICIARY: Address = Address::repeat_byte(0x03);
const PARENT_HASH: B256 = B256::repeat_byte(0x11);
const PARENT_BEACON_BLOCK_ROOT: B256 = B256::repeat_byte(0x22);

fn database() -> InMemoryDB {
    let mut database = InMemoryDB::default();
    let sender = AccountInfo { balance: U256::from(ETH_TO_WEI), ..AccountInfo::default() };
    database.insert_account_info(&SENDER, sender);
    for (address, code) in [
        (HISTORY_STORAGE_ADDRESS, &HISTORY_STORAGE_CODE),
        (BEACON_ROOTS_ADDRESS, &BEACON_ROOTS_CODE),
        (WITHDRAWAL_REQUEST_ADDRESS, &WITHDRAWAL_REQUEST_PREDEPLOY_CODE),
        (CONSOLIDATION_REQUEST_ADDRESS, &CONSOLIDATION_REQUEST_PREDEPLOY_CODE),
    ] {
        let code = Bytecode::new_legacy(code.clone());
        database.insert_account_info(&address, AccountInfo::default().with_code(code));
    }
    database
}

fn evm(spec: SpecId, gas_limit: u64) -> Evm<'static, BaseEvmTypes> {
    let block = BlockEnvExt {
        number: U256::from(1),
        timestamp: U256::from(12),
        beneficiary: BENEFICIARY,
        gas_limit: U256::from(gas_limit),
        ..BlockEnvExt::default()
    };
    Evm::new(spec, block, ethereum_tx_registry(spec), database(), Precompiles::base(spec))
}

fn ctx<'a>() -> EthBlockExecutionCtx<'a> {
    EthBlockExecutionCtx {
        parent_hash: PARENT_HASH,
        parent_beacon_block_root: Some(PARENT_BEACON_BLOCK_ROOT),
        ..EthBlockExecutionCtx::default()
    }
}

fn executor<'a>(
    spec: SpecId,
    gas_limit: u64,
    ctx: EthBlockExecutionCtx<'a>,
    rules: EthBlockRules,
) -> EthBlockExecutor<'a> {
    let evm = evm(spec, gas_limit);
    EthBlockExecutor::new(evm, ctx, rules, EthReceiptBuilder)
}

fn transfer(gas_limit: u64) -> Recovered<EthereumTxEnvelope<TxEip4844>> {
    let tx = TxLegacy {
        nonce: 0,
        gas_price: 1,
        gas_limit,
        to: TxKind::Call(RECIPIENT),
        value: U256::from(1),
        ..TxLegacy::default()
    };
    let signature = Signature::new(U256::from(1), U256::from(1), false);
    Recovered::new_unchecked(
        EthereumTxEnvelope::Legacy(Signed::new_unhashed(tx, signature)),
        SENDER,
    )
}

fn balance(state: &BundleState, address: Address) -> Option<U256> {
    state.account(&address).and_then(|account| account.info.as_ref()).map(|info| info.balance)
}

fn storage(state: &BundleState, address: Address, slot: U256) -> Option<U256> {
    state.storage(&address, slot)
}

#[test]
fn executes_prague_block() {
    let mut executor = executor(SpecId::PRAGUE, 30_000_000, ctx(), EthBlockRules::default());
    executor.apply_pre_execution_changes().unwrap();
    let gas = executor.execute_transaction(&transfer(21_000)).unwrap();
    assert_eq!(gas.tx_gas_used(), 21_000);

    let output = executor.finish().unwrap();
    let [receipt] = output.result.receipts.as_slice() else { panic!("expected one receipt") };
    assert!(receipt.status());
    assert_eq!(receipt.cumulative_gas_used(), 21_000);
    assert_eq!(output.result.gas_used, 21_000);
    assert!(output.result.requests.is_empty());

    let state = &output.state;
    assert_eq!(balance(state, RECIPIENT), Some(U256::from(1)));
    // EIP-2935 stores the parent hash at slot `(number - 1) % 8191`.
    let parent_hash = U256::from_be_bytes(PARENT_HASH.0);
    assert_eq!(storage(state, HISTORY_STORAGE_ADDRESS, U256::ZERO), Some(parent_hash));
    // EIP-4788 stores the timestamp at `timestamp % 8191` and the root 8191 slots later.
    let root = U256::from_be_bytes(PARENT_BEACON_BLOCK_ROOT.0);
    assert_eq!(storage(state, BEACON_ROOTS_ADDRESS, U256::from(12)), Some(U256::from(12)));
    assert_eq!(storage(state, BEACON_ROOTS_ADDRESS, U256::from(12 + 8191)), Some(root));
}

#[test]
fn rejects_cancun_block_without_beacon_root() {
    let ctx = EthBlockExecutionCtx { parent_beacon_block_root: None, ..ctx() };
    let mut executor = executor(SpecId::CANCUN, 30_000_000, ctx, EthBlockRules::default());
    let error = executor.apply_pre_execution_changes().unwrap_err();
    assert!(matches!(
        error,
        BlockExecutionError::Validation(BlockValidationError::MissingParentBeaconBlockRoot)
    ));
}

#[test]
fn rejects_transaction_over_block_gas() {
    let mut executor = executor(SpecId::PRAGUE, 30_000, ctx(), EthBlockRules::default());
    executor.apply_pre_execution_changes().unwrap();
    executor.execute_transaction(&transfer(21_000)).unwrap();
    let error = executor.execute_transaction(&transfer(21_000)).unwrap_err();
    assert!(matches!(
        error,
        BlockExecutionError::Validation(
            BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: 21_000,
                block_available_gas: 9_000,
            }
        )
    ));
}

#[test]
fn discards_uncommitted_transaction() {
    let mut executor = executor(SpecId::PRAGUE, 30_000_000, ctx(), EthBlockRules::default());
    executor.apply_pre_execution_changes().unwrap();
    let gas = executor
        .execute_transaction_with_commit_condition(&transfer(21_000), |_, _| CommitChanges::No)
        .unwrap();
    assert_eq!(gas, None);

    let output = executor.finish().unwrap();
    assert!(output.result.receipts.is_empty());
    assert_eq!(balance(&output.state, RECIPIENT), None);
}

#[test]
fn pays_block_and_ommer_rewards() {
    let ommer_beneficiary = Address::repeat_byte(0x04);
    let ommers = [Header { beneficiary: ommer_beneficiary, number: 0, ..Header::default() }];
    let ctx = EthBlockExecutionCtx { ommers: &ommers, ..ctx() };
    let reward = 2 * ETH_TO_WEI;
    let rules = EthBlockRules { base_block_reward: Some(reward), ..EthBlockRules::default() };
    let mut executor = executor(SpecId::PETERSBURG, 30_000_000, ctx, rules);
    executor.apply_pre_execution_changes().unwrap();

    let output = executor.finish().unwrap();
    assert_eq!(balance(&output.state, BENEFICIARY), Some(U256::from(reward + reward / 32)));
    // An ommer one block back earns 7/8 of the block reward.
    assert_eq!(balance(&output.state, ommer_beneficiary), Some(U256::from(reward * 7 / 8)));
}

#[test]
fn applies_withdrawals_from_shanghai() {
    let withdrawals = [Withdrawal { address: RECIPIENT, amount: 5, ..Withdrawal::default() }];
    let ctx = EthBlockExecutionCtx { withdrawals: Some(Cow::Borrowed(&withdrawals)), ..ctx() };
    let mut shanghai =
        executor(SpecId::SHANGHAI, 30_000_000, ctx.clone(), EthBlockRules::default());
    shanghai.apply_pre_execution_changes().unwrap();
    let output = shanghai.finish().unwrap();
    assert_eq!(balance(&output.state, RECIPIENT), Some(U256::from(5_000_000_000u64)));

    let mut merge = executor(SpecId::MERGE, 30_000_000, ctx, EthBlockRules::default());
    merge.apply_pre_execution_changes().unwrap();
    let output = merge.finish().unwrap();
    assert_eq!(balance(&output.state, RECIPIENT), None);
}

#[test]
fn reads_rules_from_hardforks() {
    let mainnet = alloy_hardforks::EthereumChainHardforks::mainnet();
    let dao = EthBlockRules::new(&mainnet, 1_920_000, None);
    assert_eq!(dao.base_block_reward, Some(5 * ETH_TO_WEI));
    assert!(dao.dao_fork_transition);
    let merged = EthBlockRules::new(&mainnet, 15_537_394, None);
    assert_eq!(merged, EthBlockRules::default());
}
