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
//! Transactions can also stream in: [`EthBlockExecutor::execute_transaction_stream`] starts each
//! transaction as soon as it arrives, while later ones are still being received, and commits the
//! block's prefix as it completes, so receiving, executing, committing and hashing the block
//! overlap.
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
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    sync::Arc,
    vec,
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
    cmp::Reverse,
    hash::Hash,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Condvar, Mutex, RwLock, mpsc},
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

/// The result of [`EthBlockExecutor::execute_transactions_parallel`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParallelExecutionOutput {
    /// Execution counters.
    pub stats: ParallelExecutionStats,
    /// The block's exact dependencies, from the state each committed transaction accessed: hints
    /// with which executing the same block again runs every transaction once. A block builder can
    /// ship them to validators.
    pub dependencies: ParallelHints,
}

/// Transaction dependencies that guide parallel scheduling: for each transaction, the earlier
/// transactions whose writes it reads.
///
/// A worker starts a transaction only once its dependencies executed, and among the transactions
/// ready, it takes the one heading the longest remaining chain of dependent gas first (critical
/// path first), which keeps long chains such as a hot contract's from finishing last. Hints only
/// order work: missing or wrong dependencies never change the result, only cost re-executions.
/// Exact dependencies, such as [`ParallelExecutionOutput::dependencies`], let every transaction
/// execute once.
///
/// The block beneficiary is left out: rewards are credited at commit, so paying fees does not
/// make transactions depend on each other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParallelHints {
    dependencies: Vec<Vec<u32>>,
}

impl ParallelHints {
    /// Creates hints from each transaction's dependencies, by index. Dependencies on the same or
    /// a later transaction are dropped, as are duplicates.
    pub fn from_dependencies(
        dependencies: impl IntoIterator<Item = impl IntoIterator<Item = u32>>,
    ) -> Self {
        let dependencies = dependencies
            .into_iter()
            .enumerate()
            .map(|(index, dependencies)| {
                let mut dependencies: Vec<_> = dependencies
                    .into_iter()
                    .filter(|&dependency| (dependency as usize) < index)
                    .collect();
                dependencies.sort_unstable();
                dependencies.dedup();
                dependencies
            })
            .collect();
        Self { dependencies }
    }

    /// Derives dependencies from the keys each transaction is expected to access, such as its
    /// sender, nonce lane, fee payer and the token balances it moves: a transaction depends on the
    /// last earlier transaction that writes each key it accesses.
    pub fn from_access_keys<K: Hash + Eq>(
        transactions: impl IntoIterator<Item = impl IntoIterator<Item = AccessKey<K>>>,
    ) -> Self {
        let mut writers = HashMap::<K, u32>::default();
        let mut dependencies = Vec::new();
        for (index, keys) in transactions.into_iter().enumerate() {
            let index = u32::try_from(index).expect("block has fewer than 2^32 transactions");
            let mut transaction = Vec::new();
            for AccessKey { key, write } in keys {
                if let Some(&writer) = writers.get(&key) {
                    transaction.push(writer);
                }
                if write {
                    writers.insert(key, index);
                }
            }
            transaction.sort_unstable();
            transaction.dedup();
            dependencies.push(transaction);
        }
        Self { dependencies }
    }

    /// Returns each transaction's dependencies, by index.
    pub fn dependencies(&self) -> &[Vec<u32>] {
        &self.dependencies
    }

    /// Returns the number of transactions in the longest dependency chain: no schedule finishes
    /// the block in fewer sequential transaction executions.
    pub fn critical_path(&self) -> usize {
        let mut depth = vec![0usize; self.dependencies.len()];
        for (index, dependencies) in self.dependencies.iter().enumerate() {
            let longest = dependencies.iter().map(|&dependency| depth[dependency as usize]).max();
            depth[index] = 1 + longest.unwrap_or_default();
        }
        depth.into_iter().max().unwrap_or_default()
    }
}

/// A key a transaction is expected to access, for [`ParallelHints::from_access_keys`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessKey<K> {
    /// Caller-defined key, such as an address, a storage slot or a nonce lane.
    pub key: K,
    /// Whether the transaction writes the key.
    pub write: bool,
}

impl<K> AccessKey<K> {
    /// A key the transaction only reads.
    pub const fn read(key: K) -> Self {
        Self { key, write: false }
    }

    /// A key the transaction writes.
    pub const fn write(key: K) -> Self {
        Self { key, write: true }
    }
}

impl<'a, T, R> EthBlockExecutor<'a, T, R>
where
    T: EvmTypes<Tx: Typed2718 + Send + Sync>,
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
    /// - `threads` is the number of worker threads. With one thread, fewer than two transactions,
    ///   or hints whose critical path spans more than half the block, transactions execute
    ///   sequentially on the calling thread.
    /// - `hints` order the workers' work by dependency and critical path (see [`ParallelHints`]).
    ///   Without them, workers take transactions in block order.
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
        hints: Option<&ParallelHints>,
        on_speculative: H,
    ) -> Result<ParallelExecutionOutput, BlockExecutionError>
    where
        Tx: ExecutableTx<Self>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T> + Sync,
        H: Fn(usize, TxStateChanges<'_>) + Sync,
    {
        let (evm_txs, txs): (Vec<_>, Vec<_>) =
            transactions.into_iter().map(ExecutableTx::into_parts).unzip();
        let mut stats = ParallelExecutionStats { transactions: txs.len(), ..Default::default() };
        let mut dependencies = DependencyTracker::new(self.evm.block_env().beneficiary);
        let serial = hints.is_some_and(|hints| hints.critical_path() * 2 > txs.len());
        if threads.get() == 1 || txs.len() < 2 || serial {
            for (index, (evm_tx, tx)) in evm_txs.into_iter().zip(txs).enumerate() {
                let output = self.execute_transaction_without_commit((evm_tx, tx))?;
                dependencies.record(index, &output.result().pending_state);
                self.commit_transaction(output)?;
            }
            stats.reexecuted = stats.transactions;
            return Ok(ParallelExecutionOutput { stats, dependencies: dependencies.finish() });
        }

        let weights: Vec<_> = txs.iter().map(|tx| tx.borrow().gas_limit()).collect();
        let priorities = priorities(&weights, hints);
        let scheduler = Scheduler::default();
        let queue = TxQueue::default();
        for (index, evm_tx) in evm_txs.into_iter().enumerate() {
            queue.push(evm_tx);
            let dependencies = hints.and_then(|hints| hints.dependencies.get(index));
            scheduler.add(priorities[index], dependencies.map_or(&[][..], Vec::as_slice));
        }
        scheduler.close();
        let store = MvStore::new(self.evm.overlay_db().cache.clone(), base);
        let (sender, receiver) = mpsc::channel::<Speculative<T>>();
        let workers = threads.get().min(txs.len());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let sender = sender.clone();
                let (store, scheduler, queue) = (&store, &scheduler, &queue);
                let (make_evm, on_speculative) = (&make_evm, &on_speculative);
                scope.spawn(move || {
                    run_worker(store, scheduler, queue, make_evm, on_speculative, &sender);
                });
            }
            drop(sender);
            let result =
                self.commit_in_order(&store, &queue, txs, &receiver, &mut stats, &mut dependencies);
            scheduler.abort();
            result
        })?;
        Ok(ParallelExecutionOutput { stats, dependencies: dependencies.finish() })
    }

    /// Executes and commits transactions in parallel as they arrive from `transactions`, with
    /// exactly the result of executing them one by one with
    /// [`BlockExecutor::execute_transaction`].
    ///
    /// Works as [`Self::execute_transactions_parallel`], except that the block need not be known
    /// up front: a worker can start a transaction as soon as it arrives, and the block's prefix
    /// commits, and streams through the state hook, as soon as it completes, while later
    /// transactions are still arriving. Iteration runs on its own thread, so `transactions` can
    /// block waiting for the next transaction, for example on a channel fed by the network or a
    /// payload builder; it ends the block by ending.
    ///
    /// Each [`StreamedTx`] can carry its dependencies on earlier transactions (see
    /// [`ParallelHints`]). Workers start ready transactions in arrival order: priorities need the
    /// whole block.
    ///
    /// When the block fails to execute, no new transaction starts, but the call returns only once
    /// `transactions` yields its next item or ends.
    pub fn execute_transaction_stream<'s, Tx, I, B, F, H>(
        &mut self,
        transactions: I,
        base: &'s B,
        make_evm: F,
        threads: NonZeroUsize,
        on_speculative: H,
    ) -> Result<ParallelExecutionOutput, BlockExecutionError>
    where
        Tx: ExecutableTx<Self, Tx: Send>,
        I: IntoIterator<Item = StreamedTx<Tx>, IntoIter: Send>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T> + Sync,
        H: Fn(usize, TxStateChanges<'_>) + Sync,
    {
        let mut stats = ParallelExecutionStats::default();
        let mut dependencies = DependencyTracker::new(self.evm.block_env().beneficiary);
        if threads.get() == 1 {
            for (index, streamed) in transactions.into_iter().enumerate() {
                let output = self.execute_transaction_without_commit(streamed.tx)?;
                dependencies.record(index, &output.result().pending_state);
                self.commit_transaction(output)?;
                stats.transactions += 1;
                stats.reexecuted += 1;
            }
            return Ok(ParallelExecutionOutput { stats, dependencies: dependencies.finish() });
        }

        let scheduler = Scheduler::default();
        let queue = TxQueue::default();
        let store = MvStore::new(self.evm.overlay_db().cache.clone(), base);
        let (sender, receiver) = mpsc::channel::<Speculative<T>>();
        let (tx_sender, tx_receiver) = mpsc::channel();
        let transactions = transactions.into_iter();
        std::thread::scope(|scope| {
            let (scheduler, queue) = (&scheduler, &queue);
            scope.spawn(move || {
                for StreamedTx { tx, dependencies } in transactions {
                    if scheduler.is_stopped() {
                        break;
                    }
                    let (evm_tx, tx) = tx.into_parts();
                    // Publish the transaction before scheduling it, and to the committer last.
                    queue.push(evm_tx);
                    scheduler.add(0, &dependencies);
                    if tx_sender.send(tx).is_err() {
                        break;
                    }
                }
                scheduler.close();
            });
            for _ in 0..threads.get() {
                let sender = sender.clone();
                let store = &store;
                let (make_evm, on_speculative) = (&make_evm, &on_speculative);
                scope.spawn(move || {
                    run_worker(store, scheduler, queue, make_evm, on_speculative, &sender);
                });
            }
            drop(sender);
            let result = self.commit_in_order(
                &store,
                queue,
                tx_receiver,
                &receiver,
                &mut stats,
                &mut dependencies,
            );
            scheduler.abort();
            result
        })?;
        Ok(ParallelExecutionOutput { stats, dependencies: dependencies.finish() })
    }

    /// Validates and commits speculative executions in block order, executing invalid ones again.
    ///
    /// `txs` yields the consensus transactions in block order; the transaction at an index is in
    /// `queue` before `txs` yields it.
    fn commit_in_order<X: Borrow<R::Transaction>>(
        &mut self,
        store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
        queue: &TxQueue<T::Tx>,
        txs: impl IntoIterator<Item = X>,
        receiver: &mpsc::Receiver<Speculative<T>>,
        stats: &mut ParallelExecutionStats,
        dependencies: &mut DependencyTracker,
    ) -> Result<(), BlockExecutionError> {
        let mut ready = Vec::<Option<Speculative<T>>>::new();
        for (index, tx) in txs.into_iter().enumerate() {
            stats.transactions = index + 1;
            while ready.get(index).is_none_or(Option::is_none) {
                let speculative =
                    receiver.recv().expect("parallel execution workers exited without a result");
                let slot = speculative.index;
                if ready.len() <= slot {
                    ready.resize_with(slot + 1, || None);
                }
                ready[slot] = Some(speculative);
            }
            let mut speculative = ready[index].take().expect("received above");
            let evm_tx = queue.get(index);
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
                    execute_transaction_without_commit(&mut self.evm, &evm_tx)
                        .map_err(|error| map_transaction_error(error, *tx.tx_hash()))?
                }
            };
            // Overwrite the speculative writes before dropping stale ones: a worker reading in
            // between must never fall through to an older version.
            let committed = store.publish(version, &result.pending_state);
            store.unpublish(version, writes.iter().filter(|write| !committed.contains(write)));
            dependencies.record(index, &result.pending_state);
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
    scheduler: &Scheduler,
    queue: &TxQueue<T::Tx>,
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
    while let Some(index) = scheduler.next() {
        let evm_tx = queue.get(index);
        let version = version(index);
        let worker_evm = evm.get_or_insert_with(|| make_evm(Box::new(EmptyDB::default())));
        worker_evm.set_defer_beneficiary_reward(true);
        reads.borrow_mut().clear();
        worker_evm.set_database(TxView { store, version, reads: &reads });
        // Speculative reads may mix versions into state no sequential execution could produce:
        // treat a panic like any other failed speculation and rebuild the EVM.
        let executed = catch_unwind(AssertUnwindSafe(|| {
            worker_evm.transact(&evm_tx).ok().map(|executed| executed.detach())
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
        scheduler.finish(index);
        let reads = core::mem::take(&mut *reads.borrow_mut());
        if sender.send(Speculative { index, outcome, reward, reads, writes }).is_err() {
            break;
        }
    }
}

/// A transaction for [`EthBlockExecutor::execute_transaction_stream`], with its dependencies on
/// earlier transactions of the block, by index.
#[derive(Clone, Debug)]
pub struct StreamedTx<Tx> {
    /// The transaction.
    pub tx: Tx,
    /// Indices of earlier transactions whose writes it reads, as in [`ParallelHints`]. Empty when
    /// unknown.
    pub dependencies: Vec<u32>,
}

impl<Tx> StreamedTx<Tx> {
    /// A transaction without known dependencies.
    pub const fn new(tx: Tx) -> Self {
        Self { tx, dependencies: Vec::new() }
    }

    /// A transaction that depends on the earlier transactions at `dependencies`.
    pub const fn with_dependencies(tx: Tx, dependencies: Vec<u32>) -> Self {
        Self { tx, dependencies }
    }
}

/// Returns each transaction's priority: its gas plus the heaviest chain of dependents after it
/// with hints, and the same for all without, so workers then take transactions in block order.
fn priorities(weights: &[u64], hints: Option<&ParallelHints>) -> Vec<u64> {
    let len = weights.len();
    let Some(hints) = hints else { return vec![0; len] };
    let mut dependents = vec![Vec::new(); len];
    for (index, dependencies) in hints.dependencies.iter().enumerate().take(len) {
        for &dependency in dependencies {
            dependents[dependency as usize].push(index);
        }
    }
    let mut priority = vec![0u64; len];
    for index in (0..len).rev() {
        let heaviest = dependents[index].iter().map(|&dependent| priority[dependent]).max();
        priority[index] = weights[index].max(1).saturating_add(heaviest.unwrap_or_default());
    }
    priority
}

/// The block's transactions, appended as they arrive and shared with the workers.
#[derive(Debug)]
struct TxQueue<Tx> {
    txs: RwLock<Vec<Arc<Recovered<Tx>>>>,
}

impl<Tx> Default for TxQueue<Tx> {
    fn default() -> Self {
        Self { txs: RwLock::default() }
    }
}

impl<Tx> TxQueue<Tx> {
    fn push(&self, tx: Recovered<Tx>) {
        self.txs.write().expect("lock poisoned").push(Arc::new(tx));
    }

    /// Returns the transaction at `index`, which was pushed.
    fn get(&self, index: usize) -> Arc<Recovered<Tx>> {
        self.txs.read().expect("lock poisoned")[index].clone()
    }
}

/// Hands transactions to workers once their dependencies executed, highest priority first, then
/// in block order. Transactions can be added while workers run.
#[derive(Debug, Default)]
struct Scheduler {
    state: Mutex<SchedulerState>,
    /// Signalled when a transaction becomes ready, the block is closed or the scheduler stops.
    wake: Condvar,
    stopped: AtomicBool,
}

#[derive(Debug, Default)]
struct SchedulerState {
    /// Transactions ready to execute, by priority, then lowest index.
    ready: BinaryHeap<(u64, Reverse<u32>)>,
    /// Each transaction's priority.
    priority: Vec<u64>,
    /// Each transaction's dependencies not yet executed.
    waiting_on: Vec<u32>,
    /// Each unexecuted transaction's waiting dependents.
    dependents: Vec<Vec<u32>>,
    /// Whether each transaction executed.
    finished: Vec<bool>,
    /// Transactions handed out.
    dispatched: usize,
    /// Whether every transaction of the block was added.
    closed: bool,
}

impl Scheduler {
    /// Adds the block's next transaction. Dependencies on itself or later transactions are
    /// ignored.
    fn add(&self, priority: u64, dependencies: &[u32]) {
        let mut state = self.state.lock().expect("lock poisoned");
        let index = state.priority.len() as u32;
        let mut waiting = 0;
        for &dependency in dependencies {
            if dependency < index && !state.finished[dependency as usize] {
                state.dependents[dependency as usize].push(index);
                waiting += 1;
            }
        }
        state.priority.push(priority);
        state.waiting_on.push(waiting);
        state.dependents.push(Vec::new());
        state.finished.push(false);
        if waiting == 0 {
            state.ready.push((priority, Reverse(index)));
            self.wake.notify_one();
        }
    }

    /// Marks the block complete: workers stop once every transaction was handed out.
    fn close(&self) {
        self.state.lock().expect("lock poisoned").closed = true;
        self.wake.notify_all();
    }

    /// Returns the next transaction to execute, waiting for one to become ready, or `None` once
    /// the block is closed and every transaction was handed out, or the scheduler stopped.
    fn next(&self) -> Option<usize> {
        let mut state = self.state.lock().expect("lock poisoned");
        loop {
            if self.is_stopped() {
                return None;
            }
            if let Some((_, Reverse(index))) = state.ready.pop() {
                state.dispatched += 1;
                return Some(index as usize);
            }
            if state.closed && state.dispatched == state.priority.len() {
                return None;
            }
            state = self.wake.wait(state).expect("lock poisoned");
        }
    }

    /// Marks a transaction executed, readying the dependents it was the last dependency of.
    fn finish(&self, index: usize) {
        let mut state = self.state.lock().expect("lock poisoned");
        state.finished[index] = true;
        let mut released = false;
        for dependent in core::mem::take(&mut state.dependents[index]) {
            let waiting = &mut state.waiting_on[dependent as usize];
            *waiting -= 1;
            if *waiting == 0 {
                let priority = state.priority[dependent as usize];
                state.ready.push((priority, Reverse(dependent)));
                released = true;
            }
        }
        let done = state.closed && state.dispatched == state.priority.len();
        drop(state);
        if released || done {
            self.wake.notify_all();
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// Stops handing out transactions.
    fn abort(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _guard = self.state.lock().expect("lock poisoned");
        self.wake.notify_all();
    }
}

/// A unit of state for dependency tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StateKey {
    Account(Address),
    Storage(Address, Word),
}

/// Derives each committed transaction's dependencies from the state it accessed.
struct DependencyTracker {
    /// Left out: rewards are credited at commit.
    beneficiary: Address,
    /// The last committed writer of each key.
    writers: HashMap<StateKey, u32>,
    /// The last committed transaction that wiped each account's storage.
    wipes: AddressMap<u32>,
    dependencies: Vec<Vec<u32>>,
}

impl DependencyTracker {
    fn new(beneficiary: Address) -> Self {
        Self {
            beneficiary,
            writers: HashMap::default(),
            wipes: AddressMap::default(),
            dependencies: Vec::new(),
        }
    }

    /// Records the transaction at `index`, which every earlier transaction precedes.
    fn record(&mut self, index: usize, pending: &PendingState) {
        let index = u32::try_from(index).expect("block has fewer than 2^32 transactions");
        let mut dependencies = Vec::new();
        for (&address, account) in &pending.accounts {
            if address == self.beneficiary {
                continue;
            }
            dependencies.extend(self.writers.get(&StateKey::Account(address)));
            if !account.storage.slots.is_empty() {
                dependencies.extend(self.wipes.get(&address));
            }
            for key in account.storage.slots.keys() {
                dependencies.extend(self.writers.get(&StateKey::Storage(address, *key)));
            }
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        self.dependencies.push(dependencies);
        for (&address, account) in &pending.accounts {
            if address == self.beneficiary {
                continue;
            }
            let deleted = account.is_changed() && account.present.is_none();
            if account.storage.wiped || account.discards_storage() || deleted {
                self.wipes.insert(address, index);
            }
            for (&key, _) in account.storage.changed_slots() {
                self.writers.insert(StateKey::Storage(address, key), index);
            }
            if account.is_changed() {
                self.writers.insert(StateKey::Account(address), index);
            }
        }
    }

    fn finish(self) -> ParallelHints {
        ParallelHints { dependencies: self.dependencies }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    fn unpublish<'w>(&self, version: u32, writes: impl IntoIterator<Item = &'w WriteKey>) {
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
