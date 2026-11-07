use super::*;
use crate::{
    Precompiles, SpecId,
    block::{
        BlockExecutionOutput, StateUpdate,
        ethereum::{EthBlockExecutionCtx, EthBlockRules, EthReceiptBuilder},
    },
    env::BlockEnvExt,
    ethereum::ethereum_tx_registry,
    evm::{BEACON_ROOTS_ADDRESS, HISTORY_STORAGE_ADDRESS, InMemoryDB},
};
use alloc::{sync::Arc, vec, vec::Vec};
use alloy_consensus::{
    EthereumTxEnvelope, ReceiptEnvelope, SignableTransaction, TxEip4844, TxLegacy,
    constants::ETH_TO_WEI,
};
use alloy_eips::{eip2935::HISTORY_STORAGE_CODE, eip4788::BEACON_ROOTS_CODE};
use alloy_primitives::{Bytes, Signature, TxKind, U256};
use std::sync::Mutex;

type Tx = Recovered<EthereumTxEnvelope<TxEip4844>>;
type Updates = Arc<Mutex<Vec<StateUpdate>>>;

const SPEC: SpecId = SpecId::PRAGUE;
const ACCOUNTS: u8 = 24;
const BENEFICIARY: Address = Address::repeat_byte(0xbe);
/// Increments slot 0.
const COUNTER: Address = Address::repeat_byte(0xc0);
const COUNTER_CODE: [u8; 10] = [0x60, 0x00, 0x54, 0x60, 0x01, 0x01, 0x60, 0x00, 0x55, 0x00];
/// Sets the caller's slot to 1.
const WRITER: Address = Address::repeat_byte(0xc1);
const WRITER_CODE: [u8; 5] = [0x60, 0x01, 0x33, 0x55, 0x00];
/// Stores the beneficiary balance in slot 0.
const COINBASE_READER: Address = Address::repeat_byte(0xc2);
const COINBASE_READER_CODE: [u8; 6] = [0x41, 0x31, 0x60, 0x00, 0x55, 0x00];

/// The pre-block state as a shared database for the workers.
struct Snapshot(Cache);

impl DynDatabaseRef for Snapshot {
    fn get_account(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        Ok(self.0.accounts.get(address).cloned().flatten())
    }

    fn get_code_by_hash(&self, code_hash: &B256) -> DbResult<Bytecode> {
        Ok(self.0.contracts.get(code_hash).cloned().unwrap_or_default())
    }

    fn get_storage(&self, address: &Address, key: &Word) -> DbResult<Word> {
        Ok(self
            .0
            .storage
            .get(address)
            .and_then(|storage| storage.slots.get(key))
            .copied()
            .unwrap_or_default())
    }

    fn get_block_hash(&self, number: &Word) -> DbResult<B256> {
        DynDatabase::get_block_hash(&mut EmptyDB::default(), number)
    }
}

fn user(index: u8) -> Address {
    Address::with_last_byte(index + 1)
}

fn database(beneficiary: Option<AccountInfo>) -> InMemoryDB {
    let mut database = InMemoryDB::default();
    for index in 0..ACCOUNTS {
        let info = AccountInfo { balance: U256::from(ETH_TO_WEI), ..AccountInfo::default() };
        database.insert_account_info(&user(index), info);
    }
    for (address, code) in [
        (HISTORY_STORAGE_ADDRESS, HISTORY_STORAGE_CODE.clone()),
        (BEACON_ROOTS_ADDRESS, BEACON_ROOTS_CODE.clone()),
        (COUNTER, Bytes::from_static(&COUNTER_CODE)),
        (WRITER, Bytes::from_static(&WRITER_CODE)),
        (COINBASE_READER, Bytes::from_static(&COINBASE_READER_CODE)),
    ] {
        let code = Bytecode::new_legacy(code);
        database.insert_account_info(&address, AccountInfo::default().with_code(code));
    }
    if let Some(info) = beneficiary {
        database.insert_account_info(&BENEFICIARY, info);
    }
    database
}

fn block() -> BlockEnvExt {
    BlockEnvExt {
        number: U256::ONE,
        timestamp: U256::from(12),
        beneficiary: BENEFICIARY,
        gas_limit: U256::from(1_000_000_000_u64),
        ..BlockEnvExt::default()
    }
}

fn evm<'x>(database: impl DynDatabase + 'x) -> Evm<'x, crate::BaseEvmTypes> {
    Evm::new(SPEC, block(), ethereum_tx_registry(SPEC), database, Precompiles::base(SPEC))
}

fn executor(database: InMemoryDB, updates: &Updates) -> EthBlockExecutor<'static> {
    let ctx = EthBlockExecutionCtx {
        parent_hash: B256::repeat_byte(0x11),
        parent_beacon_block_root: Some(B256::repeat_byte(0x22)),
        ..EthBlockExecutionCtx::default()
    };
    let mut executor =
        EthBlockExecutor::new(evm(database), ctx, EthBlockRules::default(), EthReceiptBuilder);
    let updates = updates.clone();
    executor.set_state_hook(move |update| updates.lock().unwrap().push(update));
    executor.apply_pre_execution_changes().unwrap();
    executor
}

fn tx(from: Address, nonce: u64, to: TxKind, value: u64, gas_price: u128, input: Vec<u8>) -> Tx {
    let tx = TxLegacy {
        nonce,
        gas_price,
        gas_limit: 300_000,
        to,
        value: U256::from(value),
        input: input.into(),
        ..TxLegacy::default()
    };
    let signature = Signature::new(U256::ONE, U256::ONE, false);
    Recovered::new_unchecked(EthereumTxEnvelope::Legacy(tx.into_signed(signature)), from)
}

/// Sorts each update's accounts and slots: stream order within one transaction is unspecified.
fn normalize(updates: &Updates) -> Vec<StateUpdate> {
    let mut updates = updates.lock().unwrap().clone();
    for update in &mut updates {
        update.accounts.sort_by_key(|account| account.address);
        for account in &mut update.accounts {
            account.storage.sort_by_key(|(key, _)| *key);
        }
    }
    updates
}

struct Run {
    output: BlockExecutionOutput<ReceiptEnvelope>,
    updates: Vec<StateUpdate>,
    stats: ParallelExecutionStats,
    dependencies: ParallelHints,
}

fn sequential(database: InMemoryDB, txs: &[Tx]) -> Run {
    let updates = Updates::default();
    let mut executor = executor(database, &updates);
    for tx in txs {
        executor.execute_transaction(tx).unwrap();
    }
    let output = executor.finish().unwrap();
    let stats = ParallelExecutionStats { transactions: txs.len(), ..Default::default() };
    Run { output, updates: normalize(&updates), stats, dependencies: ParallelHints::default() }
}

fn parallel(
    database: InMemoryDB,
    txs: &[Tx],
    threads: usize,
    hints: Option<&ParallelHints>,
) -> Run {
    let snapshot = Snapshot(database.cache.clone());
    let updates = Updates::default();
    let mut executor = executor(database, &updates);
    let ParallelExecutionOutput { stats, dependencies } = executor
        .execute_transactions_parallel(
            txs,
            &snapshot,
            |database| evm(database),
            NonZeroUsize::new(threads).unwrap(),
            hints,
            |_, _| {},
        )
        .unwrap();
    let output = executor.finish().unwrap();
    Run { output, updates: normalize(&updates), stats, dependencies }
}

fn assert_matches_sequential(
    database: InMemoryDB,
    txs: &[Tx],
    threads: usize,
) -> ParallelExecutionStats {
    assert_matches_sequential_with(database, txs, threads, None).stats
}

fn assert_matches_sequential_with(
    database: InMemoryDB,
    txs: &[Tx],
    threads: usize,
    hints: Option<&ParallelHints>,
) -> Run {
    let expected = sequential(database.clone(), txs);
    let actual = parallel(database, txs, threads, hints);
    assert_eq!(actual.output, expected.output, "block output differs");
    assert_eq!(actual.updates, expected.updates, "streamed state updates differ");
    assert_eq!(actual.stats.transactions, txs.len());
    assert_eq!(actual.stats.speculative + actual.stats.reexecuted, txs.len());
    assert_eq!(actual.dependencies.dependencies().len(), txs.len());
    actual
}

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random block over a small set of accounts: transfers, a hot counter slot, per-caller slots,
/// beneficiary reads and transfers, contract creations, calls into contracts created earlier in
/// the block, and contracts that self-destruct in their creating transaction.
fn random_block(seed: u64, len: usize) -> Vec<Tx> {
    random_block_with(seed, len, true)
}

/// A random block as [`random_block`], optionally without beneficiary reads and transfers.
fn random_block_with(seed: u64, len: usize, beneficiary: bool) -> Vec<Tx> {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut nonces = [0u64; ACCOUNTS as usize];
    let mut created = Vec::new();
    let mut counter_init = vec![0x69];
    counter_init.extend_from_slice(&COUNTER_CODE);
    counter_init.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x0a, 0x60, 0x16, 0xf3]);
    (0..len)
        .map(|_| {
            let sender = rng.below(ACCOUNTS as u64) as u8;
            let from = user(sender);
            let nonce = nonces[sender as usize];
            nonces[sender as usize] += 1;
            let gas_price = [0, 1, 2, 7][rng.below(4) as usize];
            let to_user = user(rng.below(ACCOUNTS as u64) as u8);
            let call = |to| TxKind::Call(to);
            let kinds = if beneficiary { 9 } else { 8 };
            match rng.below(kinds) {
                0 | 1 => tx(from, nonce, call(to_user), 1 + rng.below(1000), gas_price, vec![]),
                2 => tx(from, nonce, call(COUNTER), 0, gas_price, vec![]),
                3 => tx(from, nonce, call(WRITER), 0, gas_price, vec![]),
                4 if beneficiary => tx(from, nonce, call(COINBASE_READER), 0, gas_price, vec![]),
                5 => {
                    created.push(from.create(nonce));
                    tx(from, nonce, TxKind::Create, 0, gas_price, counter_init.clone())
                }
                6 if !created.is_empty() => {
                    let target = created[rng.below(created.len() as u64) as usize];
                    tx(from, nonce, call(target), 0, gas_price, vec![])
                }
                7 => {
                    let mut init = vec![0x73];
                    init.extend_from_slice(to_user.as_slice());
                    init.push(0xff);
                    tx(from, nonce, TxKind::Create, 5, gas_price, init)
                }
                8 => tx(from, nonce, call(BENEFICIARY), 1, gas_price, vec![]),
                _ => tx(from, nonce, call(COUNTER), 0, gas_price, vec![]),
            }
        })
        .collect()
}

#[test]
fn independent_transactions_commit_speculatively() {
    // Every transaction pays the beneficiary, but deferring the reward keeps them independent.
    let txs: Vec<_> = (0..ACCOUNTS / 2)
        .map(|index| tx(user(index), 0, TxKind::Call(user(index + ACCOUNTS / 2)), 1, 3, vec![]))
        .collect();
    let stats = assert_matches_sequential(database(None), &txs, 4);
    assert_eq!(stats.speculative, txs.len());
}

#[test]
fn dependent_transactions_match_sequential() {
    // A nonce chain, a balance chain and a hot slot.
    let (a, b, c) = (user(0), user(1), user(2));
    let txs = vec![
        tx(a, 0, TxKind::Call(b), ETH_TO_WEI as u64 / 2, 1, vec![]),
        tx(b, 0, TxKind::Call(c), ETH_TO_WEI as u64, 1, vec![]),
        tx(a, 1, TxKind::Call(COUNTER), 0, 1, vec![]),
        tx(c, 0, TxKind::Call(COUNTER), 0, 1, vec![]),
        tx(a, 2, TxKind::Call(COUNTER), 0, 1, vec![]),
        tx(b, 1, TxKind::Call(COINBASE_READER), 0, 1, vec![]),
    ];
    for threads in [2, 4, 8] {
        assert_matches_sequential(database(None), &txs, threads);
    }
}

#[test]
fn zero_fee_transactions_keep_empty_beneficiary_rules() {
    // Touching an existing empty beneficiary with a zero reward deletes it under EIP-161.
    let txs: Vec<_> = (0..8)
        .map(|index| tx(user(index), 0, TxKind::Call(user(index + 8)), 1, 0, vec![]))
        .collect();
    assert_matches_sequential(database(Some(AccountInfo::default())), &txs, 4);
    assert_matches_sequential(database(None), &txs, 4);
}

#[test]
fn random_blocks_match_sequential() {
    let (mut speculative, mut reexecuted) = (0, 0);
    for seed in 0..60 {
        let txs = random_block(seed, 96);
        for threads in [2, 5, 8] {
            let stats = assert_matches_sequential(database(None), &txs, threads);
            speculative += stats.speculative;
            reexecuted += stats.reexecuted;
        }
        let beneficiary = AccountInfo { balance: U256::from(1), ..AccountInfo::default() };
        assert_matches_sequential(database(Some(beneficiary)), &txs, 4);
    }
    // Both paths run: transactions commit speculatively, and conflicting ones execute again.
    assert!(
        speculative > 0 && reexecuted > 0,
        "{speculative} speculative, {reexecuted} re-executed"
    );
}

#[test]
fn single_thread_executes_sequentially() {
    let txs = random_block(7, 16);
    let stats = assert_matches_sequential(database(None), &txs, 1);
    assert_eq!(stats.reexecuted, txs.len());
}

#[test]
fn exact_dependencies_execute_every_transaction_once() {
    for seed in 0..40 {
        let txs = random_block_with(seed, 96, false);
        // Dependencies from one execution, as a builder would ship them.
        let first = assert_matches_sequential_with(database(None), &txs, 4, None);
        for threads in [2, 5, 8] {
            let hinted = assert_matches_sequential_with(
                database(None),
                &txs,
                threads,
                Some(&first.dependencies),
            );
            assert_eq!(hinted.stats.reexecuted, 0, "seed {seed}, {threads} threads");
            assert_eq!(hinted.dependencies, first.dependencies);
        }
    }
}

#[test]
fn sequential_execution_reports_the_same_dependencies() {
    let txs = random_block_with(3, 64, false);
    let sequential = assert_matches_sequential_with(database(None), &txs, 1, None);
    let parallel = assert_matches_sequential_with(database(None), &txs, 4, None);
    assert_eq!(sequential.dependencies, parallel.dependencies);
}

#[test]
fn wrong_hints_still_match_sequential() {
    let mut rng = Rng(42);
    for seed in 0..20 {
        let txs = random_block(seed, 96);
        let dependencies = (0..txs.len()).map(|index| {
            (0..rng.below(4)).map(|_| rng.below(index.max(1) as u64) as u32).collect::<Vec<_>>()
        });
        let hints = ParallelHints::from_dependencies(dependencies);
        assert_matches_sequential_with(database(None), &txs, 4, Some(&hints));
        let reversed = ParallelHints::from_dependencies((0..txs.len()).map(|_| vec![]));
        assert_matches_sequential_with(database(None), &txs, 4, Some(&reversed));
    }
}

#[test]
fn access_keys_derive_last_writer_dependencies() {
    let hints = ParallelHints::from_access_keys([
        vec![AccessKey::write("alice"), AccessKey::read("token")],
        vec![AccessKey::write("bob")],
        vec![AccessKey::read("alice"), AccessKey::write("token")],
        vec![AccessKey::write("alice"), AccessKey::read("token"), AccessKey::read("bob")],
    ]);
    assert_eq!(hints.dependencies(), [vec![], vec![], vec![0], vec![0, 1, 2]]);
    assert_eq!(hints.critical_path(), 3);
    let hints = ParallelHints::from_dependencies([vec![], vec![0, 0, 5], vec![1, 0, 2]]);
    assert_eq!(hints.dependencies(), [vec![], vec![0], vec![0, 1]]);
}

/// Token-style contract: moves one unit from the caller to the address in calldata word 0, then
/// hashes calldata word 1 times to stand in for the rest of a TIP-20 transfer's work. With
/// `hot_fee`, every call also increments one shared slot, as Tempo's fee accumulator and expiring
/// nonce ring pointer do today.
fn token_code(hot_fee: bool) -> Bytes {
    let mut code = vec![
        0x33, 0x54, 0x60, 0x01, 0x90, 0x03, 0x33, 0x55, // balances[caller] -= 1
        0x60, 0x00, 0x35, 0x80, 0x54, 0x60, 0x01, 0x01, 0x90, 0x55, // balances[to] += 1
    ];
    if hot_fee {
        code.extend([0x61, 0x0f, 0xee, 0x54, 0x60, 0x01, 0x01, 0x61, 0x0f, 0xee, 0x55]);
    }
    code.extend([0x60, 0x20, 0x35]);
    let start = code.len() as u8;
    code.extend([
        0x5b, 0x80, 0x15, 0x60, 0x00, 0x57, 0x60, 0x20, 0x60, 0x00, 0x20, 0x60, 0x00, 0x52, 0x60,
        0x01, 0x90, 0x03, 0x60, start, 0x56,
    ]);
    let end = code.len() as u8;
    code[start as usize + 4] = end;
    code.extend([0x5b, 0x00]);
    code.into()
}

const TOKEN: Address = Address::repeat_byte(0x7a);
const HOT_VAULT: Address = Address::repeat_byte(0xa1);
const HOT_PORTAL: Address = Address::repeat_byte(0xa2);
const HOT_SUPPLY: Address = Address::repeat_byte(0xa3);
const MIX_USERS: u64 = 1_000;

fn mix_user(index: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = 0x55;
    bytes[12..].copy_from_slice(&index.to_be_bytes());
    Address::from(bytes)
}

fn mix_database(hot_fee: bool) -> InMemoryDB {
    let mut database = database(None);
    for index in 0..MIX_USERS {
        let info = AccountInfo { balance: U256::from(ETH_TO_WEI), ..AccountInfo::default() };
        database.insert_account_info(&mix_user(index), info);
    }
    let code = Bytecode::new_legacy(token_code(hot_fee));
    database.insert_account_info(&TOKEN, AccountInfo::default().with_code(code));
    database
}

/// A block shaped like perf.tempo.xyz's `public-mix` preset: random senders among 1,000 users,
/// 65% transfers and 15% MPP opens to fresh addresses, 1% mints from one issuer into a hot supply
/// slot, 9% zone and 10% vault operations on one hot slot each. A sender's transactions chain
/// through its nonce, as they chain through its fee-token balance on Tempo.
fn public_mix_block(len: usize, work: u64) -> Vec<Tx> {
    let mut rng = Rng(0x5eed);
    let mut nonces = vec![0u64; MIX_USERS as usize];
    (0..len)
        .map(|_| {
            let roll = rng.below(100);
            let sender = if roll < 1 { 0 } else { rng.below(MIX_USERS) };
            let to = match roll {
                0 => HOT_SUPPLY,
                1..=9 => HOT_PORTAL,
                10..=19 => HOT_VAULT,
                _ => Address::from_word(B256::from(U256::from(rng.next()))),
            };
            let nonce = nonces[sender as usize];
            nonces[sender as usize] += 1;
            let mut input = to.into_word().to_vec();
            input.extend_from_slice(&U256::from(work).to_be_bytes::<32>());
            tx(mix_user(sender), nonce, TxKind::Call(TOKEN), 0, 1, input)
        })
        .collect()
}

fn time_best(
    runs: usize,
    mut f: impl FnMut() -> ParallelExecutionStats,
) -> (std::time::Duration, ParallelExecutionStats) {
    let mut best = (std::time::Duration::MAX, ParallelExecutionStats::default());
    for _ in 0..runs {
        let start = std::time::Instant::now();
        let stats = f();
        let elapsed = start.elapsed();
        if elapsed < best.0 {
            best = (elapsed, stats);
        }
    }
    best
}

/// Times sequential and parallel execution of [`public_mix_block`]:
/// `cargo test -p evm2 --features block --lib public_mix_benchmark -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn public_mix_benchmark() {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    for (work, hot_fee) in [(0, false), (64, false), (64, true)] {
        let txs = public_mix_block(4_300, work);
        let database = mix_database(hot_fee);
        let exact = parallel(database.clone(), &txs, threads, None).dependencies;
        let (sequential_time, _) = time_best(5, || {
            let updates = Updates::default();
            let mut executor = executor(database.clone(), &updates);
            for tx in &txs {
                executor.execute_transaction(tx).unwrap();
            }
            executor.finish().unwrap();
            ParallelExecutionStats::default()
        });
        std::println!(
            "work {work}, hot fee slot {hot_fee}: critical path {} of {} txs, sequential {sequential_time:?}",
            exact.critical_path(),
            txs.len()
        );
        for workers in [2, threads] {
            for (name, hints) in [("no hints", None), ("exact hints", Some(&exact))] {
                let (time, stats) = time_best(5, || {
                    let snapshot = Snapshot(database.cache.clone());
                    let updates = Updates::default();
                    let mut executor = executor(database.clone(), &updates);
                    let output = executor
                        .execute_transactions_parallel(
                            &txs,
                            &snapshot,
                            |database| evm(database),
                            NonZeroUsize::new(workers).unwrap(),
                            hints,
                            |_, _| {},
                        )
                        .unwrap();
                    executor.finish().unwrap();
                    output.stats
                });
                std::println!(
                    "  {workers} workers, {name}: {time:?} ({:.2}x), {} re-executed",
                    sequential_time.as_secs_f64() / time.as_secs_f64(),
                    stats.reexecuted
                );
            }
        }
    }
}

#[test]
fn serial_hints_execute_sequentially() {
    let txs = random_block_with(9, 32, false);
    let chain =
        ParallelHints::from_dependencies((0..txs.len() as u32).map(|index| index.checked_sub(1)));
    let run = assert_matches_sequential_with(database(None), &txs, 4, Some(&chain));
    assert_eq!(run.stats.reexecuted, txs.len());
}
