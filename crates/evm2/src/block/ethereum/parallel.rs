//! Optimistic parallel execution of a block's transactions.
//!
//! [`EthBlockExecutor::execute_transactions_parallel`] runs a block's transactions speculatively on
//! worker threads and commits them on the calling thread in block order, so the result is exactly
//! that of sequential execution:
//!
//! 1. Workers take transactions in block order and execute each on their own [`Evm`] over a
//!    multi-version view of the block's state: a read of transaction `i` sees the latest write any
//!    transaction `j < i` published, committed or still speculative, and otherwise the state before
//!    the first transaction. Every value read is recorded, and the transaction's writes are
//!    published to the view as soon as it finishes, so later transactions can read them before it
//!    commits.
//! 2. The calling thread commits transactions in order. Once every earlier transaction committed,
//!    transaction `i` is valid if each value it read still equals the committed value: execution is
//!    deterministic in its reads, so it then produced exactly the sequential result. A valid
//!    transaction is committed as executed; an invalid one is executed again on the executor's own
//!    EVM, which holds the sequential state, and its committed writes replace the speculative ones.
//!
//! Commits stream: each transaction's state update reaches the executor's state hook (see
//! [`BlockExecutor::set_state_hook`]) the moment its prefix of the block is committed, in block
//! order, rather than when the block finishes, so a state-root task can hash a transaction's
//! changes while later ones still execute. The `on_speculative` callback additionally sees every
//! speculative write-set as soon as a worker finishes it, before validation, for prefetching the
//! trie nodes the commit will need.
//!
//! The block beneficiary is the one key nearly every transaction writes. Workers therefore run with
//! [`Evm::set_defer_beneficiary_reward`]: a transaction that does not read the beneficiary itself
//! never loads it, and the reward is credited when the transaction commits. A transaction that
//! does read it executes again sequentially.

use super::{
    EthBlockExecutor, EthTransactionResultWithState,
    execution::{execute_transaction_without_commit, map_transaction_error},
};
use crate::{
    Evm, EvmFeatures, EvmTypes, TxResultWithState, TxStateChanges,
    block::{BlockExecutionError, BlockExecutor, ExecutableTx, ReceiptBuilder},
    bytecode::Bytecode,
    evm::{
        AccountFlags, AccountInfo, Cache, DbResult, DynDatabase, DynDatabaseRef, EmptyDB,
        PendingState,
    },
    interpreter::{Host, Word},
};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use alloy_consensus::{
    Transaction,
    transaction::{Recovered, TransactionEnvelope, TxHashRef},
};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{
    Address, B256,
    map::{AddressMap, B256Map, HashMap},
};
use core::{
    borrow::Borrow,
    cell::RefCell,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{RwLock, mpsc},
};

/// Number of lock shards of the multi-version account and storage maps.
const SHARDS: usize = 64;

/// Counters of one parallel block execution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParallelExecutionStats {
    /// Transactions committed.
    pub transactions: usize,
    /// Transactions committed from their speculative execution.
    pub speculative: usize,
    /// Transactions executed again on the executor because their speculative execution read
    /// stale state, failed, or read the beneficiary.
    pub reexecuted: usize,
}

impl<'a, T, R> EthBlockExecutor<'a, T, R>
where
    T: EvmTypes<Tx: Typed2718 + Sync>,
    R: ReceiptBuilder<Transaction: Transaction + TxHashRef>,
    TxResultWithState<T>: Send,
{
    /// Executes and commits `transactions` in parallel, with exactly the result of executing
    /// them one by one with [`BlockExecutor::execute_transaction`].
    ///
    /// See the [module documentation](self) for how it works.
    ///
    /// - `base` is the state the executor's EVM reads beneath its overlay, shared with the workers:
    ///   it must hold the same state as the executor's backing database.
    /// - `make_evm` builds a worker EVM over the database it is given, with the executor's spec,
    ///   configuration and block environment. Its database is replaced before every transaction,
    ///   and beneficiary reward deferral is turned on.
    /// - `threads` is the number of worker threads. With one thread, or fewer than two
    ///   transactions, transactions execute sequentially on the calling thread.
    /// - `on_speculative` observes each speculative execution's changes, by transaction index, as
    ///   soon as a worker finishes it and before it is validated: hints for prefetching, not
    ///   committed state. Committed changes stream through the executor's state hook.
    ///
    /// Call it after [`BlockExecutor::apply_pre_execution_changes`] and before any transaction of
    /// the block. Do not attach a block access list for reads: a block with one executes in
    /// parallel from it directly (see the `bal_parallel` example).
    pub fn execute_transactions_parallel<'s, Tx, B, F, H>(
        &mut self,
        transactions: impl IntoIterator<Item = Tx>,
        base: &'s B,
        make_evm: F,
        threads: NonZeroUsize,
        on_speculative: H,
    ) -> Result<ParallelExecutionStats, BlockExecutionError>
    where
        Tx: ExecutableTx<Self>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T> + Sync,
        H: Fn(usize, TxStateChanges<'_>) + Sync,
    {
        let (evm_txs, txs): (Vec<_>, Vec<_>) =
            transactions.into_iter().map(ExecutableTx::into_parts).unzip();
        let mut stats = ParallelExecutionStats { transactions: txs.len(), ..Default::default() };
        if threads.get() == 1 || txs.len() < 2 {
            for (evm_tx, tx) in evm_txs.into_iter().zip(txs) {
                self.execute_transaction((evm_tx, tx))?;
            }
            stats.reexecuted = stats.transactions;
            return Ok(stats);
        }

        let store = MvStore::new(self.evm.overlay_db().cache.clone(), base);
        let next = AtomicUsize::new(0);
        let abort = AtomicBool::new(false);
        let (sender, receiver) = mpsc::channel::<Speculative<T>>();
        let workers = threads.get().min(txs.len());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let sender = sender.clone();
                let (store, next, abort, evm_txs) = (&store, &next, &abort, &evm_txs);
                let (make_evm, on_speculative) = (&make_evm, &on_speculative);
                scope.spawn(move || {
                    run_worker(store, next, abort, evm_txs, make_evm, on_speculative, &sender);
                });
            }
            drop(sender);
            let result = self.commit_in_order(&store, &evm_txs, &txs, &receiver, &mut stats);
            abort.store(true, Ordering::Relaxed);
            result
        })?;
        Ok(stats)
    }

    /// Validates and commits speculative executions in block order, executing invalid ones again.
    fn commit_in_order<X: Borrow<R::Transaction>>(
        &mut self,
        store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
        evm_txs: &[Recovered<T::Tx>],
        txs: &[X],
        receiver: &mpsc::Receiver<Speculative<T>>,
        stats: &mut ParallelExecutionStats,
    ) -> Result<(), BlockExecutionError> {
        let mut ready: Vec<Option<Speculative<T>>> = (0..txs.len()).map(|_| None).collect();
        for (index, (evm_tx, tx)) in evm_txs.iter().zip(txs).enumerate() {
            while ready[index].is_none() {
                let speculative =
                    receiver.recv().expect("parallel execution workers exited without a result");
                let slot = speculative.index;
                ready[slot] = Some(speculative);
            }
            let mut speculative = ready[index].take().expect("received above");
            let tx = tx.borrow();
            let version = version(index);

            self.set_transaction_block_access_index();
            self.validate_transaction_gas_limit(tx.gas_limit())?;
            let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
            self.validate_transaction_blob_gas(blob_gas_used)?;

            let writes = core::mem::take(&mut speculative.writes);
            let result = match self.accept_speculative(store, version, speculative) {
                Some(result) => {
                    stats.speculative += 1;
                    result
                }
                None => {
                    stats.reexecuted += 1;
                    execute_transaction_without_commit(&mut self.evm, evm_tx)
                        .map_err(|error| map_transaction_error(error, *tx.tx_hash()))?
                }
            };
            store.unpublish(version, &writes);
            store.publish(version, &result.pending_state);
            self.commit_transaction(EthTransactionResultWithState::new(
                result,
                tx.tx_type(),
                blob_gas_used,
            ))?;
        }
        Ok(())
    }

    /// Returns the speculative result of the transaction at `version` with its beneficiary reward
    /// credited, or `None` when it must execute again. Every earlier transaction has committed.
    fn accept_speculative(
        &mut self,
        store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
        version: u32,
        speculative: Speculative<T>,
    ) -> Option<TxResultWithState<T>> {
        let Speculative { outcome: Some(mut outcome), reward, reads, .. } = speculative else {
            return None;
        };
        if !reads.validate(store, version).unwrap_or(false) {
            return None;
        }
        if !self.evm.feature(EvmFeatures::FEE_CHARGE) {
            return Some(outcome);
        }
        // The transaction read or wrote the beneficiary itself, so the reward would land on
        // state it already changed: let the sequential EVM order the two.
        let beneficiary = self.evm.block_env().beneficiary;
        if outcome.pending_state.accounts.contains_key(&beneficiary) {
            return None;
        }
        // Credit the reward as the default settlement would have: load, touch, add.
        let (_, original) = store.account(&beneficiary, version).ok()?;
        let current = match &original {
            Some(info) if reward.is_zero() => {
                // Touching an existing empty account deletes it under EIP-161.
                if info.is_empty() && self.evm.feature(EvmFeatures::EIP161) {
                    return None;
                }
                Some(info.clone())
            }
            None if reward.is_zero() => None,
            Some(info) => {
                let mut info = info.clone();
                info.balance = info.balance.wrapping_add(reward);
                Some(info)
            }
            None => Some(AccountInfo { balance: reward, ..AccountInfo::empty() }),
        };
        outcome.pending_state.insert_account(beneficiary, original, current);
        if let Some(account) = outcome.pending_state.accounts.get_mut(&beneficiary) {
            account.flags.insert(AccountFlags::TOUCHED);
        }
        Some(outcome)
    }
}

/// Returns the multi-version store version holding the writes of the transaction at `index`.
/// Version 0 is the state before the first transaction.
#[inline]
fn version(index: usize) -> u32 {
    u32::try_from(index + 1).expect("block has fewer than 2^32 transactions")
}

/// Executes transactions speculatively until none are left or the committer stops.
fn run_worker<T, F, H>(
    store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
    next: &AtomicUsize,
    abort: &AtomicBool,
    evm_txs: &[Recovered<T::Tx>],
    make_evm: &F,
    on_speculative: &H,
    sender: &mpsc::Sender<Speculative<T>>,
) where
    T: EvmTypes<Tx: Typed2718>,
    F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T>,
    H: Fn(usize, TxStateChanges<'_>),
{
    let reads = RefCell::new(ReadSet::default());
    let mut evm = None;
    while !abort.load(Ordering::Relaxed) {
        let index = next.fetch_add(1, Ordering::Relaxed);
        let Some(evm_tx) = evm_txs.get(index) else { break };
        let version = version(index);
        let worker_evm = evm.get_or_insert_with(|| make_evm(Box::new(EmptyDB::default())));
        worker_evm.set_defer_beneficiary_reward(true);
        reads.borrow_mut().clear();
        worker_evm.set_database(TxView { store, version, reads: &reads });
        // Speculative reads may mix versions into state no sequential execution could produce:
        // treat a panic like any other failed speculation and rebuild the EVM.
        let executed = catch_unwind(AssertUnwindSafe(|| {
            worker_evm.transact(evm_tx).ok().map(|executed| executed.detach())
        }));
        let (outcome, reward) = match executed {
            Ok(outcome) => (outcome, worker_evm.deferred_beneficiary_reward().unwrap_or_default()),
            Err(_) => {
                evm = None;
                (None, Default::default())
            }
        };
        let writes = match &outcome {
            Some(outcome) => {
                let writes = store.publish(version, &outcome.pending_state);
                on_speculative(index, outcome.pending_state.changes());
                writes
            }
            None => Vec::new(),
        };
        let reads = core::mem::take(&mut *reads.borrow_mut());
        if sender.send(Speculative { index, outcome, reward, reads, writes }).is_err() {
            break;
        }
    }
}

/// One speculative execution sent from a worker to the committer.
struct Speculative<T: EvmTypes> {
    /// Transaction index in the block.
    index: usize,
    /// Detached result, or `None` when execution failed.
    outcome: Option<TxResultWithState<T>>,
    /// Beneficiary reward withheld from the result.
    reward: Word,
    /// Values the execution read from the multi-version store.
    reads: ReadSet,
    /// Keys the execution published to the multi-version store.
    writes: Vec<WriteKey>,
}

/// Where a recorded read was served from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadOrigin {
    /// The state before the first transaction.
    Base,
    /// The write of the transaction at this version.
    Version(u32),
}

/// The values one speculative execution read.
#[derive(Debug, Default)]
struct ReadSet {
    accounts: Vec<(Address, ReadOrigin, Option<AccountInfo>)>,
    storage: Vec<(Address, Word, ReadOrigin, Word)>,
}

impl ReadSet {
    fn clear(&mut self) {
        self.accounts.clear();
        self.storage.clear();
    }

    /// Returns whether every read still sees the same value at `version`.
    fn validate(
        &self,
        store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
        version: u32,
    ) -> DbResult<bool> {
        for (address, origin, value) in &self.accounts {
            if !store.validate_account(address, version, *origin, value)? {
                return Ok(false);
            }
        }
        for (address, key, origin, value) in &self.storage {
            if !store.validate_storage(address, key, version, *origin, *value)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// A key a transaction published to the multi-version store.
#[derive(Clone, Copy, Debug)]
enum WriteKey {
    Account(Address),
    Storage(Address, Word),
    Wipe(Address),
}

/// Versions of one key: the value each writing transaction left, by version.
type Versions<V> = BTreeMap<u32, V>;

/// One lock shard of account versions.
type AccountShard = RwLock<AddressMap<Versions<Option<AccountInfo>>>>;

/// One lock shard of storage slot versions.
type StorageShard = RwLock<HashMap<(Address, Word), Versions<Word>>>;

/// Multi-version state of a block in execution.
///
/// Transaction `i` publishes its writes at version `i + 1`. Reading at version `v` returns the
/// latest write below `v`, then the state before the first transaction: the executor's overlay at
/// the start (pre-execution changes), then the base database. A storage wipe at a version shadows
/// every slot write of that account below it, as in [`CacheDB`](crate::evm::CacheDB).
struct MvStore<'s, B: ?Sized> {
    /// Executor overlay before the first transaction.
    initial: Cache,
    /// Backing database beneath the overlay.
    base: &'s B,
    accounts: Box<[AccountShard]>,
    storage: Box<[StorageShard]>,
    wipes: RwLock<AddressMap<BTreeSet<u32>>>,
    has_wipes: AtomicBool,
    contracts: RwLock<B256Map<Bytecode>>,
}

impl<'s, B: DynDatabaseRef + Sync + ?Sized> MvStore<'s, B> {
    fn new(initial: Cache, base: &'s B) -> Self {
        Self {
            initial,
            base,
            accounts: (0..SHARDS).map(|_| RwLock::default()).collect(),
            storage: (0..SHARDS).map(|_| RwLock::default()).collect(),
            wipes: RwLock::default(),
            has_wipes: AtomicBool::new(false),
            contracts: RwLock::default(),
        }
    }

    #[inline]
    fn account_shard(&self, address: &Address) -> &AccountShard {
        &self.accounts[address[19] as usize % SHARDS]
    }

    #[inline]
    fn storage_shard(&self, address: &Address, key: &Word) -> &StorageShard {
        &self.storage[(address[19] as usize ^ key.as_limbs()[0] as usize) % SHARDS]
    }

    /// Returns the latest account write below `version`.
    fn account_version(
        &self,
        address: &Address,
        version: u32,
    ) -> Option<(u32, Option<AccountInfo>)> {
        let shard = self.account_shard(address).read().expect("lock poisoned");
        let (&at, info) = shard.get(address)?.range(..version).next_back()?;
        Some((at, info.clone()))
    }

    /// Returns the account before the first transaction.
    fn account_initial(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        match self.initial.accounts.get(address) {
            Some(info) => Ok(info.clone()),
            None => self.base.get_account(address),
        }
    }

    /// Reads an account at `version`.
    fn account(
        &self,
        address: &Address,
        version: u32,
    ) -> DbResult<(ReadOrigin, Option<AccountInfo>)> {
        if let Some((at, info)) = self.account_version(address, version) {
            return Ok((ReadOrigin::Version(at), info));
        }
        Ok((ReadOrigin::Base, self.account_initial(address)?))
    }

    /// Returns the latest slot write or wipe below `version`; a wipe reads as zero.
    fn storage_version(&self, address: &Address, key: &Word, version: u32) -> Option<(u32, Word)> {
        let slot = {
            let shard = self.storage_shard(address, key).read().expect("lock poisoned");
            shard.get(&(*address, *key)).and_then(|versions| {
                versions.range(..version).next_back().map(|(&at, &value)| (at, value))
            })
        };
        let wipe = if self.has_wipes.load(Ordering::Acquire) {
            let wipes = self.wipes.read().expect("lock poisoned");
            wipes.get(address).and_then(|wipes| wipes.range(..version).next_back().copied())
        } else {
            None
        };
        match (slot, wipe) {
            (Some(slot), None) => Some(slot),
            // A transaction that wipes and then writes a slot publishes both at its version.
            (Some((at, value)), Some(wipe)) if at >= wipe => Some((at, value)),
            (_, Some(wipe)) => Some((wipe, Word::ZERO)),
            (None, None) => None,
        }
    }

    /// Returns a slot before the first transaction.
    fn storage_initial(&self, address: &Address, key: &Word) -> DbResult<Word> {
        if let Some(storage) = self.initial.storage.get(address) {
            if let Some(value) = storage.slots.get(key) {
                return Ok(*value);
            }
            if storage.wiped {
                return Ok(Word::ZERO);
            }
        }
        if let Some(None) = self.initial.accounts.get(address) {
            return Ok(Word::ZERO);
        }
        self.base.get_storage(address, key)
    }

    /// Reads a storage slot at `version`.
    fn storage(&self, address: &Address, key: &Word, version: u32) -> DbResult<(ReadOrigin, Word)> {
        if let Some((at, value)) = self.storage_version(address, key, version) {
            return Ok((ReadOrigin::Version(at), value));
        }
        Ok((ReadOrigin::Base, self.storage_initial(address, key)?))
    }

    /// Returns whether an account read at `version` still sees `value`.
    fn validate_account(
        &self,
        address: &Address,
        version: u32,
        origin: ReadOrigin,
        value: &Option<AccountInfo>,
    ) -> DbResult<bool> {
        match (self.account_version(address, version), origin) {
            (Some((_, current)), _) => Ok(current == *value),
            // Nothing below the version wrote the account, then or now.
            (None, ReadOrigin::Base) => Ok(true),
            (None, ReadOrigin::Version(_)) => Ok(self.account_initial(address)? == *value),
        }
    }

    /// Returns whether a storage read at `version` still sees `value`.
    fn validate_storage(
        &self,
        address: &Address,
        key: &Word,
        version: u32,
        origin: ReadOrigin,
        value: Word,
    ) -> DbResult<bool> {
        match (self.storage_version(address, key, version), origin) {
            (Some((_, current)), _) => Ok(current == value),
            (None, ReadOrigin::Base) => Ok(true),
            (None, ReadOrigin::Version(_)) => Ok(self.storage_initial(address, key)? == value),
        }
    }

    /// Reads bytecode by hash: deployed in this block, before it, or in the base database.
    fn code(&self, code_hash: &B256) -> DbResult<Bytecode> {
        if let Some(code) = self.contracts.read().expect("lock poisoned").get(code_hash) {
            return Ok(code.clone());
        }
        if let Some(code) = self.initial.contracts.get(code_hash) {
            return Ok(code.clone());
        }
        self.base.get_code_by_hash(code_hash)
    }

    /// Publishes a transaction's writes at `version`, as [`CacheDB`](crate::evm::CacheDB) commits
    /// them, and returns the keys written.
    fn publish(&self, version: u32, pending: &PendingState) -> Vec<WriteKey> {
        let mut writes = Vec::new();
        for (&address, account) in &pending.accounts {
            let storage = &account.storage;
            let mut wiped = storage.wiped || account.discards_storage();
            if account.is_changed() && account.present.is_none() {
                wiped = true;
            }
            if wiped {
                self.has_wipes.store(true, Ordering::Release);
                self.wipes
                    .write()
                    .expect("lock poisoned")
                    .entry(address)
                    .or_default()
                    .insert(version);
                writes.push(WriteKey::Wipe(address));
            }
            for (&key, slot) in storage.changed_slots() {
                let mut shard = self.storage_shard(&address, &key).write().expect("lock poisoned");
                shard.entry((address, key)).or_default().insert(version, slot.current);
                writes.push(WriteKey::Storage(address, key));
            }
            if let Some((code_hash, code)) = account.changed_code() {
                self.contracts
                    .write()
                    .expect("lock poisoned")
                    .entry(code_hash)
                    .or_insert_with(|| code.clone());
            }
            if account.is_changed() {
                let info = account.present.as_ref().map(AccountInfo::clone_no_code);
                let mut shard = self.account_shard(&address).write().expect("lock poisoned");
                shard.entry(address).or_default().insert(version, info);
                writes.push(WriteKey::Account(address));
            }
        }
        writes
    }

    /// Removes the writes a speculative execution published at `version`.
    fn unpublish(&self, version: u32, writes: &[WriteKey]) {
        for write in writes {
            match write {
                WriteKey::Account(address) => {
                    let mut shard = self.account_shard(address).write().expect("lock poisoned");
                    if let Some(versions) = shard.get_mut(address) {
                        versions.remove(&version);
                    }
                }
                WriteKey::Storage(address, key) => {
                    let mut shard =
                        self.storage_shard(address, key).write().expect("lock poisoned");
                    if let Some(versions) = shard.get_mut(&(*address, *key)) {
                        versions.remove(&version);
                    }
                }
                WriteKey::Wipe(address) => {
                    if let Some(wipes) = self.wipes.write().expect("lock poisoned").get_mut(address)
                    {
                        wipes.remove(&version);
                    }
                }
            }
        }
    }
}

/// A worker's database for one transaction: the multi-version store at the transaction's
/// version, recording every value served.
struct TxView<'m, 's, B: ?Sized> {
    store: &'m MvStore<'s, B>,
    version: u32,
    reads: &'m RefCell<ReadSet>,
}

impl<B: DynDatabaseRef + Sync + ?Sized> DynDatabase for TxView<'_, '_, B> {
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let (origin, info) = self.store.account(address, self.version)?;
        self.reads.borrow_mut().accounts.push((*address, origin, info.clone()));
        Ok(info)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        // Code is content-addressed: a hash reads the same bytes at every version.
        self.store.code(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let (origin, value) = self.store.storage(address, key, self.version)?;
        self.reads.borrow_mut().storage.push((*address, *key, origin, value));
        Ok(value)
    }

    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        match self.store.initial.block_hashes.get(number) {
            Some(hash) => Ok(*hash),
            None => self.store.base.get_block_hash(number),
        }
    }
}

#[cfg(test)]
mod tests;
