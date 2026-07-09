//! Executing transactions independently against an already-built EIP-7928 Block
//! Access List (BAL).
//!
//! With a BAL attached, reads are served from it at the transaction's block access
//! index (`i + 1` for transaction `i`), so transaction `i` sees the post-state of
//! transactions `0..i` without those transactions having been committed to this
//! EVM. Every execution only needs the pre-block database plus the shared BAL,
//! which is what makes the transactions of a block executable in parallel.
//!
//! Each worker thread sends its detached execution output (result + state diff) back
//! to the main thread, which folds the diffs into a fresh BAL and checks it against
//! the block's BAL — the validation half of EIP-7928.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_eip7928::BalanceChange;
use alloy_primitives::{Address, TxKind, U256};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResultWithState,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, Bal, BlockAccessIndex, InMemoryDB},
};
use std::sync::Arc;

const CALLER: Address = Address::with_last_byte(0xaa);
const ALICE: Address = Address::with_last_byte(0xbb);
const BOB: Address = Address::with_last_byte(0xcc);

fn main() {
    // Transaction 1 spends money ALICE only receives in transaction 0, so it cannot
    // execute against pre-block state alone.
    let transactions = [transfer(CALLER, ALICE, 1_000_000, 0), transfer(ALICE, BOB, 400_000, 0)];

    // Sequential execution with the builder enabled produces the block's BAL (see the
    // `bal_build` example). A validator would instead decode it from the block body.
    let bal = build_bal(&transactions);
    assert_eq!(
        bal.accounts.get(&ALICE).unwrap().account_info.balance.changes,
        vec![
            BalanceChange::new(idx(1), U256::from(1_000_000)),
            BalanceChange::new(idx(2), U256::from(600_000)),
        ]
    );

    // Without the BAL, transaction 1 on pre-block state fails: ALICE has no funds yet.
    let mut evm = pre_block_evm();
    assert!(evm.transact(&transactions[1]).is_err());

    // With the BAL attached, each transaction executes on its own EVM over the same
    // pre-block database, one worker thread per transaction. Reads positioned at
    // index `i + 1` see all writes recorded at indices `<= i`, i.e. exactly the
    // transaction's pre-state, so no thread needs another thread's committed state.
    //
    // Each worker detaches its transaction into an owned result + state diff and sends
    // it back to main through its join handle. Main is the single consumer: it folds
    // the diffs into a fresh BAL in transaction order (`Bal` writes must be appended
    // with ascending indices) and checks the rebuilt BAL against the block's BAL,
    // which is exactly how a validator confirms the block's BAL is correct.
    let bal = Arc::new(bal);
    let mut rebuilt = Bal::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = transactions
            .iter()
            .enumerate()
            .map(|(i, tx)| {
                let bal = bal.clone();
                s.spawn(move || execute_with_bal(bal, i as u64 + 1, tx))
            })
            .collect();
        for (i, handle) in handles.into_iter().enumerate() {
            let output = handle.join().expect("worker thread panicked");
            assert!(output.result.status);
            for (address, change) in &output.state_changes.accounts {
                rebuilt.update_account(idx(i as u64 + 1), *address, change);
            }
            println!(
                "transaction {i} executed in parallel, gas used {}",
                output.result.tx_gas_used()
            );
        }
    });
    assert_eq!(rebuilt, *bal, "BAL rebuilt from parallel execution must match the block's BAL");
    println!("rebuilt BAL from parallel outputs matches the block's BAL");
}

/// Executes one transaction over pre-block state with reads served from `bal` at
/// `index`. A read the BAL does not cover returns `ErrorCode::BAL_NOT_COVERED`,
/// which during validation means the BAL is invalid (use
/// `set_allow_bal_db_fallback(true)` to instead fall through to the database, e.g.
/// for RPC calls on BAL-positioned state).
///
/// The executed transaction is detached into an owned [`TxResultWithState`] so it can
/// leave the worker thread; nothing is committed to this EVM, which is dropped here.
fn execute_with_bal(bal: Arc<Bal>, index: u64, tx: &RecoveredTxEnvelope) -> TxResultWithState {
    let mut evm = pre_block_evm();
    evm.state_mut().set_bal(bal);
    evm.state_mut().set_bal_index(BlockAccessIndex::new(index));
    evm.transact(tx).expect("transaction should execute").detach()
}

fn build_bal(transactions: &[RecoveredTxEnvelope]) -> Bal {
    let mut evm = pre_block_evm();
    evm.state_mut().enable_bal_builder();
    evm.state_mut().reset_bal_index();
    for tx in transactions {
        evm.state_mut().bump_bal_index();
        let result = evm.transact(tx).expect("transaction should execute").commit();
        assert!(result.status);
    }
    evm.state_mut().take_bal_builder().expect("builder was enabled")
}

fn pre_block_evm() -> Evm<'static, BaseEvmTypes> {
    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &CALLER,
        AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
    );

    let spec = SpecId::AMSTERDAM;
    Evm::new(
        spec,
        BlockEnv::default(),
        ethereum_tx_registry(spec),
        database,
        Precompiles::base(spec),
    )
}

fn transfer(from: Address, to: Address, value: u64, nonce: u64) -> RecoveredTxEnvelope {
    RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
        TxLegacy {
            to: TxKind::Call(to),
            value: U256::from(value),
            gas_limit: 300_000,
            nonce,
            ..Default::default()
        },
        from,
    ))
}

const fn idx(index: u64) -> BlockAccessIndex {
    BlockAccessIndex::new(index)
}
