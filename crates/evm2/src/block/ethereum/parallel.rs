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
//!
//! Host code such as a precompile can give other shared keys the same treatment. Its blind adds
//! to a storage slot ([`Host::sadd`]), such as a fee accumulator's, are published as deltas
//! rather than read: a later transaction that reads the slot sees the latest value written below
//! its version plus every delta since, while transactions that only add never conflict, and each
//! add resolves against the committed value when its transaction commits. Its deferred storage
//! operations ([`State::defer_storage_op`](crate::evm::State::defer_storage_op)), such as a
//! ring buffer insert, run at commit against the committed state.

use super::{
    EthBlockExecutor, EthTransactionResultWithState,
    execution::{execute_transaction_without_commit, map_transaction_error},
};
use crate::{
    Evm, EvmFeatures, EvmTypes, TxResultWithState, TxStateChanges,
    block::{BlockExecutionError, BlockExecutor, ExecutableTx, ReceiptBuilder},
    bytecode::Bytecode,
    evm::{AccountFlags, AccountInfo, Cache, DbResult, DynDatabase, DynDatabaseRef, PendingState},
    interpreter::{Host, Word},
};
use alloc::{
    boxed::Box,
    collections::{BTreeSet, BinaryHeap},
    vec,
    vec::Vec,
};
use alloy_consensus::{
    Transaction,
    transaction::{Recovered, TransactionEnvelope, TxHashRef},
};
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY,
    map::{AddressMap, B256Map, HashMap, HashSet},
};
use core::{
    borrow::Borrow,
    cell::{Cell, RefCell},
    cmp::Reverse,
    hash::Hash,
    num::NonZeroUsize,
    ops::Deref,
    ptr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence},
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Condvar, Mutex, OnceLock, RwLock, mpsc},
    thread::Thread,
};

/// Number of lock shards of the multi-version account and storage maps.
const SHARDS: usize = 64;

/// Number of buckets of a [`Buckets`], each twice the size of the one before.
const BUCKETS: usize = 32;

/// Number of elements of the first bucket of a [`Buckets`].
const FIRST_BUCKET: usize = 64;

/// Times an idle worker polls for results to execute again before parking.
const TAIL_SPINS: u32 = 1 << 8;

/// How long an idle worker parks between polls for results to execute again.
const TAIL_PARK: core::time::Duration = core::time::Duration::from_micros(20);

/// Times the committer polls for the next result before parking.
const COMMIT_SPINS: u32 = 1 << 10;

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
/// make transactions depend on each other. So are storage slots a transaction changes only through
/// blind adds or deferred storage operations, which resolve at commit, though a later transaction
/// that reads such a slot depends on every transaction that changed it so since its last write.
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

        let queue = TxQueue::default();
        let scheduler = if let Some(hints) = hints {
            let weights: Vec<_> = txs.iter().map(|tx| tx.borrow().gas_limit()).collect();
            let priorities = priorities(&weights, Some(hints));
            let scheduler = Scheduler::default();
            for (index, evm_tx) in evm_txs.into_iter().enumerate() {
                queue.push(evm_tx);
                let dependencies = hints.dependencies.get(index);
                scheduler.add(priorities[index], dependencies.map_or(&[][..], Vec::as_slice));
            }
            scheduler.close();
            scheduler
        } else {
            for evm_tx in evm_txs {
                queue.push(evm_tx);
            }
            Scheduler::in_order(txs.len())
        };
        let store = MvStore::new(self.evm.overlay_db().cache.clone(), base);
        let workers = threads.get().min(txs.len());
        let results = Results::new(workers);
        std::thread::scope(|scope| {
            for worker in 0..workers {
                let (store, scheduler, queue, results) = (&store, &scheduler, &queue, &results);
                let (make_evm, on_speculative) = (&make_evm, &on_speculative);
                scope.spawn(move || {
                    run_worker(store, scheduler, queue, make_evm, on_speculative, results, worker);
                });
            }
            let result =
                self.commit_in_order(&store, &queue, txs, &results, &mut stats, &mut dependencies);
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
        let results = Results::new(threads.get());
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
            for worker in 0..threads.get() {
                let (store, results) = (&store, &results);
                let (make_evm, on_speculative) = (&make_evm, &on_speculative);
                scope.spawn(move || {
                    run_worker(store, scheduler, queue, make_evm, on_speculative, results, worker);
                });
            }
            let result = self.commit_in_order(
                &store,
                queue,
                tx_receiver,
                &results,
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
        results: &Results<Speculative<T>>,
        stats: &mut ParallelExecutionStats,
        dependencies: &mut DependencyTracker,
    ) -> Result<(), BlockExecutionError> {
        for (index, tx) in txs.into_iter().enumerate() {
            stats.transactions = index + 1;
            let Speculative { worker, outcome, reward, mut reads, mut writes } =
                results.take(index).expect("parallel execution workers exited without a result");
            let evm_tx = queue.get(index);
            let tx = tx.borrow();
            let version = version(index);

            self.set_transaction_block_access_index();
            self.validate_transaction_gas_limit(tx.gas_limit())?;
            let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
            self.validate_transaction_blob_gas(blob_gas_used)?;

            let pending =
                match self.accept_speculative(outcome, reward, &mut reads, evm_tx.signer()) {
                    Some(result) => {
                        // The worker published every write but the credited reward, which workers
                        // never read, and what resolving its deferred storage at commit wrote.
                        stats.speculative += 1;
                        // Commit as `commit_transaction` does, but keep the pending state to hand
                        // its allocations back to the worker.
                        let TxResultWithState { result, pending_state, .. } = result;
                        // Later transactions read the balances it changed as deltas over earlier
                        // ones at what it committed, not at what its execution saw.
                        for (address, ..) in &reads.rebase {
                            if let Some(account) = pending_state.accounts.get(address)
                                && account.is_changed()
                            {
                                store.republish_account(
                                    version,
                                    *address,
                                    account.present.as_ref(),
                                );
                            }
                        }
                        if !pending_state.deferred.resolved.is_empty() {
                            store.publish_resolved(version, &pending_state);
                        }
                        dependencies.record(index, &pending_state);
                        self.commit_pending_state(&pending_state);
                        self.record_receipt(tx.tx_type(), blob_gas_used, result);
                        pending_state
                    }
                    None => {
                        stats.reexecuted += 1;
                        let result = execute_transaction_without_commit(&mut self.evm, evm_tx)
                            .map_err(|error| map_transaction_error(error, *tx.tx_hash()))?;
                        // Overwrite the speculative writes before dropping stale ones: a worker
                        // reading in between must never fall through to an older version.
                        let mut committed = Vec::new();
                        store.publish(version, &result.pending_state, &mut committed);
                        // Later results may have read the stale writes: have idle workers check
                        // them again.
                        results.revalidate.fetch_min(index + 1, Ordering::AcqRel);
                        let committed = committed.into_iter().collect::<HashSet<_>>();
                        store.unpublish(
                            version,
                            writes.iter().filter(|write| !committed.contains(*write)),
                        );
                        dependencies.record(index, &result.pending_state);
                        self.commit_transaction(EthTransactionResultWithState::new(
                            result,
                            tx.tx_type(),
                            blob_gas_used,
                        ))?;
                        PendingState::default()
                    }
                };
            reads.clear();
            writes.clear();
            results.recycle(worker, Buffers { reads, writes, pending });
        }
        Ok(())
    }

    /// Returns a speculative result with its beneficiary `reward` credited, or `None` when it
    /// must execute again. Every earlier transaction has committed.
    fn accept_speculative(
        &mut self,
        outcome: Option<TxResultWithState<T>>,
        reward: Word,
        reads: &mut ReadSet,
        sender: Address,
    ) -> Option<TxResultWithState<T>> {
        let mut outcome = outcome?;
        if !reads.validate(&mut self.evm, &outcome.pending_state, &sender).unwrap_or(false) {
            return None;
        }
        // Apply the balance changes it made without reading the balance exactly over the balances
        // earlier transactions left.
        for &(address, read, now) in &reads.rebase {
            let Some(account) = outcome.pending_state.accounts.get_mut(&address) else { continue };
            let (Some(original), Some(present)) = (&mut account.original, &mut account.present)
            else {
                return None;
            };
            if original.balance != read {
                return None;
            }
            original.balance = now;
            present.balance = present.balance.wrapping_sub(read).wrapping_add(now);
            // Emptiness decides EIP-161 deletion, which the execution settled on its balances.
            if present.is_empty() {
                return None;
            }
        }
        // Resolve blind adds and deferred storage operations against the committed state, as the
        // transaction's end would have: its execution did not read what they change.
        if outcome.pending_state.has_deferred_storage()
            && self.evm.state_mut().resolve_pending_state(&mut outcome.pending_state).is_err()
        {
            return None;
        }
        if !self.evm.feature(EvmFeatures::FEE_CHARGE) {
            return Some(outcome);
        }
        // The transaction read or wrote the beneficiary itself, so the reward would land on
        // state it already changed: let the sequential EVM order the two.
        let beneficiary = self.evm.block_env().beneficiary;
        if outcome.pending_state.accounts.contains_key(&beneficiary)
            || reads.accounts.iter().any(|(address, _)| *address == beneficiary)
        {
            return None;
        }
        // Credit the reward as the default settlement would have: load, touch, add.
        let original = self.evm.read_account_info(&beneficiary).ok()?;
        let current = match &original {
            Some(info) if reward.is_zero() => {
                // Touching an existing empty account deletes it under EIP-161.
                if info.is_empty() && self.evm.feature(EvmFeatures::EIP161) {
                    return None;
                }
                Some(info.clone())
            }
            // Before EIP-161, touching an absent account creates it empty: let the sequential
            // EVM do it.
            None if reward.is_zero() && !self.evm.feature(EvmFeatures::EIP161) => return None,
            None if reward.is_zero() => None,
            Some(info) => {
                let mut info = info.clone();
                info.balance = info.balance.wrapping_add(reward);
                // A reward that wraps the balance to zero leaves an empty account, which EIP-161
                // deletes.
                if info.is_empty() && self.evm.feature(EvmFeatures::EIP161) {
                    return None;
                }
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
    results: &Results<Speculative<T>>,
    worker: usize,
) where
    T: EvmTypes<Tx: Typed2718>,
    F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T>,
    H: Fn(usize, TxStateChanges<'_>),
{
    let _exit = WorkerExit(results);
    // A panic outside the guarded execution, such as in `make_evm` or `on_speculative`, must not
    // leave transactions waiting on one this worker never finishes: stop the block instead.
    let _abort = AbortOnUnwind(scheduler);
    let view_version = Cell::new(0);
    let reads = RefCell::new(ReadSet::default());
    let mut stash = Vec::new();
    let mut evm = None;
    while let Some(index) = scheduler.next() {
        let evm_tx = queue.get(index);
        let version = version(index);
        let worker_evm = evm.get_or_insert_with(|| {
            let view = TxView { store, version: &view_version, reads: &reads };
            let mut evm = make_evm(Box::new(view));
            evm.set_defer_beneficiary_reward(true);
            evm.set_defer_storage_resolution(true);
            evm.set_track_balance_reads(true);
            evm
        });
        // Values cached at the previous transaction's version are stale; code is
        // content-addressed and stays valid.
        let cache = &mut worker_evm.overlay_db_mut().cache;
        cache.accounts.clear();
        cache.storage.clear();
        view_version.set(version);
        let Buffers { reads: pooled, mut writes, pending } = results.buffers(worker, &mut stash);
        *reads.borrow_mut() = pooled;
        // Install a recycled pending state's allocations, emptied, for the transaction to fill.
        // Reset the withheld reward, so that a transaction settled without the default settlement
        // reads none rather than the previous transaction's.
        worker_evm.set_defer_beneficiary_reward(true);
        let state = worker_evm.state_mut();
        state.set_pending_state(pending);
        state.clear_transaction_state();
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
        if let Some(outcome) = &outcome {
            store.publish(version, &outcome.pending_state, &mut writes);
            on_speculative(index, outcome.pending_state.changes());
        }
        scheduler.finish(index);
        let reads = core::mem::take(&mut *reads.borrow_mut());
        results.put(index, Speculative { worker, outcome, reward, reads, writes });
    }

    // Every transaction was handed out, and the committer is still behind: execute again, ahead
    // of it, the results whose reads earlier executions have since overwritten, so that it can
    // commit them instead of executing them itself.
    let Some(evm) = &mut evm else { return };
    let mut idle = 0;
    while !scheduler.is_stopped() {
        let index = results.revalidate.load(Ordering::Acquire);
        if index >= queue.len.load(Ordering::Acquire) {
            // Wait for the committer to execute a transaction again, which can make later results
            // stale, or to finish, without taking a core it may need.
            idle += 1;
            if idle < TAIL_SPINS {
                core::hint::spin_loop();
            } else {
                std::thread::park_timeout(TAIL_PARK);
            }
            continue;
        }
        if results
            .revalidate
            .compare_exchange_weak(index, index + 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            continue;
        }
        idle = 0;
        if index <= results.committing.load(Ordering::Acquire) {
            continue;
        }
        let mut stop = false;
        results.update(index, |speculative| {
            let version = version(index);
            if let Some(outcome) = &speculative.outcome
                && speculative.reads.validate_at(
                    store,
                    version,
                    &outcome.pending_state,
                    &queue.get(index).signer(),
                )
            {
                return;
            }
            let evm_tx = queue.get(index);
            let cache = &mut evm.overlay_db_mut().cache;
            cache.accounts.clear();
            cache.storage.clear();
            view_version.set(version);
            let Buffers { reads: pooled, mut writes, pending } =
                results.buffers(worker, &mut stash);
            *reads.borrow_mut() = pooled;
            evm.set_defer_beneficiary_reward(true);
            let state = evm.state_mut();
            state.set_pending_state(pending);
            state.clear_transaction_state();
            let executed = catch_unwind(AssertUnwindSafe(|| {
                evm.transact(evm_tx).ok().map(|executed| executed.detach())
            }));
            let Ok(outcome) = executed else {
                // The EVM may be left inconsistent: keep the stale result and stop.
                reads.borrow_mut().clear();
                stop = true;
                return;
            };
            let reward = evm.deferred_beneficiary_reward().unwrap_or_default();
            if let Some(outcome) = &outcome {
                store.publish(version, &outcome.pending_state, &mut writes);
                on_speculative(index, outcome.pending_state.changes());
            }
            store.unpublish(
                version,
                speculative.writes.iter().filter(|write| !writes.contains(write)),
            );
            let reads = core::mem::take(&mut *reads.borrow_mut());
            let stale = core::mem::replace(
                speculative,
                Speculative { worker, outcome, reward, reads, writes },
            );
            let Speculative { worker: owner, reads: mut old_reads, writes: mut old_writes, .. } =
                stale;
            old_reads.clear();
            old_writes.clear();
            results.recycle(
                owner,
                Buffers { reads: old_reads, writes: old_writes, ..Default::default() },
            );
        });
        if stop {
            return;
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

/// The block's transactions, appended as they arrive and shared with the workers without locking.
#[derive(Debug)]
struct TxQueue<Tx> {
    txs: Buckets<OnceLock<Recovered<Tx>>>,
    len: AtomicUsize,
}

impl<Tx> Default for TxQueue<Tx> {
    fn default() -> Self {
        Self { txs: Buckets::default(), len: AtomicUsize::new(0) }
    }
}

impl<Tx> TxQueue<Tx> {
    /// Appends the next transaction. Only one thread pushes.
    fn push(&self, tx: Recovered<Tx>) {
        let index = self.len.fetch_add(1, Ordering::Relaxed);
        if self.txs.get(index).set(tx).is_err() {
            unreachable!("each index is pushed once");
        }
    }

    /// Returns the transaction at `index`, which was pushed.
    fn get(&self, index: usize) -> &Recovered<Tx> {
        self.txs.get(index).get().expect("transaction was pushed")
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
    /// The next transaction and the block's length when every transaction is known up front and
    /// none waits on another: transactions are then handed out in block order without locking.
    in_order: Option<(AtomicUsize, usize)>,
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
    /// A scheduler that hands out the `len` transactions of a closed block in block order.
    fn in_order(len: usize) -> Self {
        Self { in_order: Some((AtomicUsize::new(0), len)), ..Default::default() }
    }

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
        if let Some((next, len)) = &self.in_order {
            let index = next.fetch_add(1, Ordering::Relaxed);
            return (index < *len && !self.is_stopped()).then_some(index);
        }
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
        if self.in_order.is_some() {
            return;
        }
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

/// Derives each committed transaction's dependencies from the state it accessed.
struct DependencyTracker {
    /// Left out: rewards are credited at commit.
    beneficiary: Address,
    /// The last committed writer of each account.
    accounts: AddressMap<u32>,
    /// The last committed writer of each storage slot.
    storage: HashMap<(Address, Word), u32>,
    /// The committed transactions that changed each slot since its last writer only through blind
    /// adds or deferred storage operations: they do not depend on each other, but a reader of the
    /// slot depends on all of them.
    deferred_writers: HashMap<(Address, Word), Vec<u32>>,
    /// The last committed transaction that wiped each account's storage.
    wipes: AddressMap<u32>,
    dependencies: Vec<Vec<u32>>,
}

impl DependencyTracker {
    fn new(beneficiary: Address) -> Self {
        Self {
            beneficiary,
            accounts: AddressMap::default(),
            storage: HashMap::default(),
            deferred_writers: HashMap::default(),
            wipes: AddressMap::default(),
            dependencies: Vec::new(),
        }
    }

    /// Records the transaction at `index`, which every earlier transaction precedes.
    fn record(&mut self, index: usize, pending: &PendingState) {
        let index = u32::try_from(index).expect("block has fewer than 2^32 transactions");
        // Slots only the transaction's end loaded, to resolve its deferred storage.
        let resolved = pending.deferred.resolved.iter().copied().collect::<HashSet<_>>();
        let resolved = |key: &(Address, Word)| resolved.contains(key);
        let mut dependencies = Vec::new();
        // Each key is visited once, so reading its last writer and then recording this transaction
        // as the writer, in one lookup, is the same as reading every key before writing any.
        for (&address, account) in &pending.accounts {
            if address == self.beneficiary {
                continue;
            }
            let changed = account.is_changed();
            dependencies.extend(if changed {
                self.accounts.insert(address, index)
            } else {
                self.accounts.get(&address).copied()
            });
            if !account.storage.slots.is_empty() {
                dependencies.extend(self.wipes.get(&address));
            }
            if account.storage.wiped
                || account.discards_storage()
                || changed && account.present.is_none()
            {
                self.wipes.insert(address, index);
            }
            // Changed slots come in the order of all slots: they are the same map, filtered.
            let mut changed_slots = account.storage.changed_slots().map(|(key, _)| key).peekable();
            for key in account.storage.slots.keys() {
                let written = changed_slots.next_if(|&changed| ptr::eq(changed, key)).is_some();
                let key = (address, *key);
                if resolved(&key) {
                    if written {
                        self.deferred_writers.entry(key).or_default().push(index);
                    }
                    continue;
                }
                dependencies.extend(if written {
                    self.storage.insert(key, index)
                } else {
                    self.storage.get(&key).copied()
                });
                if self.deferred_writers.is_empty() {
                    continue;
                }
                if written {
                    dependencies.extend(self.deferred_writers.remove(&key).into_iter().flatten());
                } else {
                    dependencies.extend(self.deferred_writers.get(&key).into_iter().flatten());
                }
            }
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        self.dependencies.push(dependencies);
    }

    fn finish(self) -> ParallelHints {
        ParallelHints { dependencies: self.dependencies }
    }
}

/// One speculative execution sent from a worker to the committer.
struct Speculative<T: EvmTypes> {
    /// Index of the worker that executed it.
    worker: usize,
    /// Detached result, or `None` when execution failed.
    outcome: Option<TxResultWithState<T>>,
    /// Beneficiary reward withheld from the result.
    reward: Word,
    /// Values the execution read from the multi-version store.
    reads: ReadSet,
    /// Keys the execution published to the multi-version store.
    writes: Vec<WriteKey>,
}

/// The values one speculative execution read.
#[derive(Debug, Default)]
struct ReadSet {
    accounts: Vec<(Address, Option<AccountInfo>)>,
    storage: Vec<(Address, Word, Word)>,
    /// Accounts whose committed balance differs from the one read, which the execution did not
    /// read exactly: the balance read and the committed one, filled by [`Self::validate`].
    rebase: Vec<(Address, Word, Word)>,
}

impl ReadSet {
    fn clear(&mut self) {
        self.accounts.clear();
        self.storage.clear();
        self.rebase.clear();
    }

    /// Returns whether every read sees the same value in the committed state of `evm`, the
    /// executor's EVM, which holds the sequential state, or only a balance the execution of
    /// `pending`, sent by `sender`, did not read exactly, which it records to rebase.
    fn validate<T: EvmTypes>(
        &mut self,
        evm: &mut Evm<'_, T>,
        pending: &PendingState,
        sender: &Address,
    ) -> DbResult<bool> {
        let Self { accounts, storage, rebase } = self;
        rebase.clear();
        for (address, value) in accounts.iter() {
            let now = evm.read_account_info(address)?;
            if now != *value {
                let Some(floor) = delta_floor(pending, sender, address) else { return Ok(false) };
                let (Some(read), Some(committed)) = (value, &now) else { return Ok(false) };
                if !balance_only_change(read, committed, floor) {
                    return Ok(false);
                }
                match rebase.iter().find(|(rebased, ..)| rebased == address) {
                    // Reads of one account at different versions cannot both be rebased.
                    Some(&(_, balance, _)) if balance != read.balance => return Ok(false),
                    Some(_) => {}
                    None => rebase.push((*address, read.balance, committed.balance)),
                }
            }
            // A worker reads every slot of an account it read as absent as zero, without asking
            // the store, while the executor still sees committed slots of such an account.
            if value.is_none()
                && evm
                    .overlay_db()
                    .cache
                    .storage
                    .get(address)
                    .is_some_and(|storage| !storage.slots.is_empty())
            {
                return Ok(false);
            }
        }
        for (address, key, value) in storage.iter() {
            if evm.read_storage(address, key)? != *value {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl ReadSet {
    /// Returns whether every read still sees the same value in `store` at `version`: the latest
    /// writes of earlier transactions, committed or speculative.
    fn validate_at<B: DynDatabaseRef + Sync + ?Sized>(
        &self,
        store: &MvStore<'_, B>,
        version: u32,
        pending: &PendingState,
        sender: &Address,
    ) -> bool {
        self.accounts.iter().all(|(address, value)| {
            store.account(address, version).is_ok_and(|info| {
                let info = info.as_ref().map(AccountInfo::clone_no_code);
                info == *value
                    || matches!((value, &info), (Some(read), Some(now))
                        if delta_floor(pending, sender, address)
                            .is_some_and(|floor| balance_only_change(read, now, floor)))
            })
        }) && self.storage.iter().all(|(address, key, value)| {
            store.storage(address, key, version).is_ok_and(|v| v == *value)
        })
    }
}

/// Returns the lowest balance of `address` the execution of `pending`, sent by `sender`, needs
/// when it did not read the balance exactly, or `None` when it did. The sender's balance pays
/// for gas, which the execution checks exactly.
fn delta_floor(pending: &PendingState, sender: &Address, address: &Address) -> Option<Word> {
    let reads = &pending.balance_reads;
    if address == sender || reads.observed.contains(address) {
        return None;
    }
    Some(reads.floors.get(address).copied().unwrap_or_default())
}

/// Returns whether an account read as `read` and now `now` differs at most in a balance that
/// still covers `floor`, with the same emptiness, which EIP-161 and new-account gas depend on.
fn balance_only_change(read: &AccountInfo, now: &AccountInfo, floor: Word) -> bool {
    if now.balance < floor || read.is_empty() != now.is_empty() {
        return false;
    }
    let mut rebased = read.clone();
    rebased.balance = now.balance;
    rebased == *now
}

/// A key a transaction published to the multi-version store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum WriteKey {
    Account(Address),
    Storage(Address, Word),
    Wipe(Address),
}

/// One lock shard of account versions.
type AccountShard = RwLock<AddressMap<Versions<Option<AccountInfo>>>>;

/// One lock shard of storage slot versions.
type StorageShard = RwLock<HashMap<(Address, Word), Versions<SlotWrite>>>;

/// A transaction's write to a storage slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotWrite {
    /// The value it left.
    Value(Word),
    /// A wrapping sum of blind adds to whatever value it found.
    Delta(Word),
}

/// Multi-version state of a block in execution.
///
/// Transaction `i` publishes its writes at version `i + 1`. Reading at version `v` returns the
/// latest write below `v`, then the state before the first transaction: the executor's overlay at
/// the start (pre-execution changes), then the base database. A storage wipe at a version shadows
/// every slot write of that account below it, as in [`CacheDB`](crate::evm::CacheDB). A slot
/// reads as its latest value below `v` plus every blind-add delta published above that value.
struct MvStore<'s, B: ?Sized> {
    /// Executor overlay before the first transaction.
    initial: Cache,
    /// Backing database beneath the overlay.
    base: &'s B,
    accounts: Box<[CachePadded<AccountShard>]>,
    storage: Box<[CachePadded<StorageShard>]>,
    wipes: RwLock<AddressMap<BTreeSet<u32>>>,
    has_wipes: AtomicBool,
    contracts: RwLock<B256Map<Bytecode>>,
}

impl<'s, B: DynDatabaseRef + Sync + ?Sized> MvStore<'s, B> {
    fn new(initial: Cache, base: &'s B) -> Self {
        Self {
            initial,
            base,
            accounts: (0..SHARDS).map(|_| CachePadded::default()).collect(),
            storage: (0..SHARDS).map(|_| CachePadded::default()).collect(),
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
    fn account_version(&self, address: &Address, version: u32) -> Option<Option<AccountInfo>> {
        let shard = self.account_shard(address).read().expect("lock poisoned");
        let (_, info) = shard.get(address)?.latest_below(version)?;
        Some(info.clone())
    }

    /// Returns the account before the first transaction.
    fn account_initial(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        match self.initial.accounts.get(address) {
            Some(info) => Ok(info.clone()),
            None => self.base.get_account(address),
        }
    }

    /// Reads an account at `version`.
    fn account(&self, address: &Address, version: u32) -> DbResult<Option<AccountInfo>> {
        match self.account_version(address, version) {
            Some(info) => Ok(info),
            None => self.account_initial(address),
        }
    }

    /// Reads a storage slot at `version`: the latest value written below it, or zero after a later
    /// wipe, then the state before the first transaction, plus every delta published above that.
    fn storage(&self, address: &Address, key: &Word, version: u32) -> DbResult<Word> {
        let wipe = if self.has_wipes.load(Ordering::Acquire) {
            let wipes = self.wipes.read().expect("lock poisoned");
            wipes.get(address).and_then(|wipes| wipes.range(..version).next_back().copied())
        } else {
            None
        };
        let (mut value, mut delta) = (None, Word::ZERO);
        {
            let shard = self.storage_shard(address, key).read().expect("lock poisoned");
            if let Some(versions) = shard.get(&(*address, *key)) {
                for (at, write) in versions.below(version) {
                    // A transaction that wipes and then writes a slot publishes both at its
                    // version.
                    if wipe.is_some_and(|wipe| at < wipe) {
                        break;
                    }
                    match *write {
                        SlotWrite::Value(written) => {
                            value = Some(written);
                            break;
                        }
                        SlotWrite::Delta(added) => delta = delta.wrapping_add(added),
                    }
                }
            }
        }
        let value = match (value, wipe) {
            (Some(value), _) => value,
            (None, Some(_)) => Word::ZERO,
            (None, None) => self.storage_initial(address, key)?,
        };
        Ok(value.wrapping_add(delta))
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
    /// them, and appends the keys written to `writes`.
    fn publish(&self, version: u32, pending: &PendingState, writes: &mut Vec<WriteKey>) {
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
                shard
                    .entry((address, key))
                    .or_default()
                    .insert(version, SlotWrite::Value(slot.current));
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
        // Unresolved blind adds publish as deltas, unless the transaction loaded the slot too.
        for (key, pending_delta) in &pending.deferred.deltas {
            if pending_delta.delta.is_zero() {
                continue;
            }
            let (address, key) = (key.address(), key.key());
            let loaded =
                pending.accounts.get(&address).and_then(|account| account.storage.slots.get(&key));
            let write = match loaded {
                Some(slot) => {
                    SlotWrite::Value(slot.value.current.wrapping_add(pending_delta.delta))
                }
                None => SlotWrite::Delta(pending_delta.delta),
            };
            let mut shard = self.storage_shard(&address, &key).write().expect("lock poisoned");
            shard.entry((address, key)).or_default().insert(version, write);
            writes.push(WriteKey::Storage(address, key));
        }
    }

    /// Removes the writes a speculative execution published at `version`.
    fn unpublish<'w>(&self, version: u32, writes: impl IntoIterator<Item = &'w WriteKey>) {
        for write in writes {
            match write {
                WriteKey::Account(address) => {
                    let mut shard = self.account_shard(address).write().expect("lock poisoned");
                    if let Some(versions) = shard.get_mut(address) {
                        versions.remove(version);
                    }
                }
                WriteKey::Storage(address, key) => {
                    let mut shard =
                        self.storage_shard(address, key).write().expect("lock poisoned");
                    if let Some(versions) = shard.get_mut(&(*address, *key)) {
                        versions.remove(version);
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

    /// Replaces the account a committed transaction published at `version` with the one it
    /// committed.
    fn republish_account(&self, version: u32, address: Address, info: Option<&AccountInfo>) {
        let mut shard = self.account_shard(&address).write().expect("lock poisoned");
        shard.entry(address).or_default().insert(version, info.map(AccountInfo::clone_no_code));
    }

    /// Publishes at `version` the slots that resolving a committed transaction's deferred storage
    /// changed: the values of blind adds it never read and the slots its deferred operations wrote,
    /// which its speculative execution could not publish.
    fn publish_resolved(&self, version: u32, pending: &PendingState) {
        for &(address, key) in &pending.deferred.resolved {
            if let Some(account) = pending.accounts.get(&address)
                && let Some(slot) = account.storage.slots.get(&key)
                && (slot.value.is_changed() || account.storage.wiped)
            {
                let mut shard = self.storage_shard(&address, &key).write().expect("lock poisoned");
                shard
                    .entry((address, key))
                    .or_default()
                    .insert(version, SlotWrite::Value(slot.value.current));
            }
        }
    }
}

/// A worker's database for one transaction: the multi-version store at the transaction's
/// version, recording every value served.
struct TxView<'m, 's, B: ?Sized> {
    store: &'m MvStore<'s, B>,
    /// The version of the worker's current transaction.
    version: &'m Cell<u32>,
    reads: &'m RefCell<ReadSet>,
}

impl<B: DynDatabaseRef + Sync + ?Sized> DynDatabase for TxView<'_, '_, B> {
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let mut info = self.store.account(address, self.version.get())?;
        // Record the account as the executor's database returns it, which maps a zero code hash
        // to the empty one.
        if let Some(info) = &mut info
            && info.code_hash.is_zero()
        {
            info.code_hash = KECCAK256_EMPTY;
        }
        let read = info.as_ref().map(AccountInfo::clone_no_code);
        self.reads.borrow_mut().accounts.push((*address, read));
        Ok(info)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        // Code is content-addressed: a hash reads the same bytes at every version.
        self.store.code(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let value = self.store.storage(address, key, self.version.get())?;
        self.reads.borrow_mut().storage.push((*address, *key, value));
        Ok(value)
    }

    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        match self.store.initial.block_hashes.get(number) {
            Some(hash) => Ok(*hash),
            None => self.store.base.get_block_hash(number),
        }
    }
}

/// A sequence that grows by buckets allocated on first use, so threads index it without locking.
struct Buckets<T> {
    buckets: [OnceLock<Box<[T]>>; BUCKETS],
}

impl<T> Default for Buckets<T> {
    fn default() -> Self {
        Self { buckets: core::array::from_fn(|_| OnceLock::new()) }
    }
}

impl<T> core::fmt::Debug for Buckets<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Buckets").finish_non_exhaustive()
    }
}

impl<T: Default> Buckets<T> {
    /// Returns the element at `index`, allocating its bucket on first use.
    #[inline]
    fn get(&self, index: usize) -> &T {
        let bucket = (index / FIRST_BUCKET + 1).ilog2() as usize;
        let start = FIRST_BUCKET * ((1 << bucket) - 1);
        let elements = self.buckets[bucket]
            .get_or_init(|| (0..FIRST_BUCKET << bucket).map(|_| T::default()).collect());
        &elements[index - start]
    }
}

/// Speculative results by transaction index, handed from the workers to the committer.
struct Results<S> {
    slots: Buckets<ResultSlot<S>>,
    /// The committing thread, unparked when the result it waits for arrives or a worker exits.
    committer: Thread,
    /// Index of the result the committer is about to park for, or `usize::MAX`.
    waiting: AtomicUsize,
    /// Workers still running.
    workers: AtomicUsize,
    /// Each worker's buffers, handed back by the committer.
    pools: Box<[CachePadded<Mutex<Vec<Buffers>>>]>,
    /// Index of the transaction the committer is committing: workers leave it and every earlier
    /// result alone.
    committing: CachePadded<AtomicUsize>,
    /// Index of the next result for an idle worker to validate against the latest writes.
    revalidate: CachePadded<AtomicUsize>,
}

impl<S> Results<S> {
    /// Results produced by `workers` threads for the calling thread to take.
    fn new(workers: usize) -> Self {
        Self {
            slots: Buckets::default(),
            committer: std::thread::current(),
            waiting: AtomicUsize::new(usize::MAX),
            workers: AtomicUsize::new(workers),
            pools: (0..workers).map(|_| CachePadded::default()).collect(),
            committing: CachePadded(AtomicUsize::new(0)),
            revalidate: CachePadded(AtomicUsize::new(0)),
        }
    }

    /// Stores the result of the transaction at `index`, waking the committer if it waits for it.
    fn put(&self, index: usize, result: S) {
        let slot = self.slots.get(index);
        *slot.value.lock().expect("lock poisoned") = Some(result);
        slot.ready.store(true, Ordering::Release);
        // Pairs with the fence in `take`: either it sees the result or this sees it waiting.
        fence(Ordering::SeqCst);
        if self.waiting.load(Ordering::Relaxed) == index {
            self.committer.unpark();
        }
    }

    /// Takes the result of the transaction at `index`, waiting for it, or returns `None` once
    /// every worker exited without producing it.
    fn take(&self, index: usize) -> Option<S> {
        self.committing.store(index, Ordering::Relaxed);
        let slot = self.slots.get(index);
        let mut spins = 0;
        while !slot.ready.load(Ordering::Acquire) {
            if spins < COMMIT_SPINS {
                spins += 1;
                core::hint::spin_loop();
                continue;
            }
            self.waiting.store(index, Ordering::Relaxed);
            fence(Ordering::SeqCst);
            if slot.ready.load(Ordering::Acquire) {
                break;
            }
            if self.workers.load(Ordering::Acquire) == 0 {
                // A worker stores its last result before it exits.
                if !slot.ready.load(Ordering::Acquire) {
                    self.waiting.store(usize::MAX, Ordering::Relaxed);
                    return None;
                }
                break;
            }
            std::thread::park();
        }
        self.waiting.store(usize::MAX, Ordering::Relaxed);
        slot.value.lock().expect("lock poisoned").take()
    }

    /// Runs `f` on the result at `index` while it holds the result's lock, if the result is
    /// stored, the committer has not taken it and no other worker holds it. The committer waits
    /// for `f` to finish before it takes the result.
    fn update(&self, index: usize, f: impl FnOnce(&mut S)) {
        let slot = self.slots.get(index);
        if !slot.ready.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut value) = slot.value.try_lock() else { return };
        if let Some(result) = value.as_mut() {
            f(result);
        }
    }

    /// Returns buffers for `worker`, with empty reads and writes, from its `stash`, which it
    /// refills with every buffer handed back since it last ran out.
    fn buffers(&self, worker: usize, stash: &mut Vec<Buffers>) -> Buffers {
        if stash.is_empty() {
            core::mem::swap(stash, &mut *self.pools[worker].lock().expect("lock poisoned"));
        }
        stash.pop().unwrap_or_default()
    }

    /// Hands buffers, with emptied reads and writes, back to the `worker` that filled them.
    fn recycle(&self, worker: usize, buffers: Buffers) {
        self.pools[worker].lock().expect("lock poisoned").push(buffers);
    }
}

/// The allocations of one speculative execution, which the committer hands back to the worker that
/// made them, so that they are reused instead of allocated by one thread and freed by another.
#[derive(Debug, Default)]
struct Buffers {
    reads: ReadSet,
    writes: Vec<WriteKey>,
    /// A committed pending state, whose account and storage maps the worker's EVM reuses.
    pending: PendingState,
}

/// One transaction's result in [`Results`].
struct ResultSlot<S> {
    /// Whether `value` was stored.
    ready: AtomicBool,
    value: Mutex<Option<S>>,
}

impl<S> Default for ResultSlot<S> {
    fn default() -> Self {
        Self { ready: AtomicBool::new(false), value: Mutex::new(None) }
    }
}

/// Stops the scheduler when a worker unwinds.
struct AbortOnUnwind<'s>(&'s Scheduler);

impl Drop for AbortOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.abort();
        }
    }
}

/// Counts a worker out of [`Results`] when it exits, even by unwinding.
struct WorkerExit<'r, S>(&'r Results<S>);

impl<S> Drop for WorkerExit<'_, S> {
    fn drop(&mut self) {
        self.0.workers.fetch_sub(1, Ordering::AcqRel);
        self.0.committer.unpark();
    }
}

/// Versions of one key: the value each writing transaction left, by ascending version. Most keys
/// have a single version, kept without allocating.
#[derive(Debug, Default)]
enum Versions<V> {
    #[default]
    Empty,
    One(u32, V),
    Many(Vec<(u32, V)>),
}

impl<V> Versions<V> {
    /// Sets the value at `version`.
    fn insert(&mut self, version: u32, value: V) {
        *self = match core::mem::take(self) {
            Self::Empty => Self::One(version, value),
            Self::One(at, _) if at == version => Self::One(version, value),
            Self::One(at, current) => {
                let mut versions = Vec::with_capacity(4);
                versions.push((at, current));
                insert_version(&mut versions, version, value);
                Self::Many(versions)
            }
            Self::Many(mut versions) => {
                insert_version(&mut versions, version, value);
                Self::Many(versions)
            }
        };
    }

    /// Removes the value at `version`, if any.
    fn remove(&mut self, version: u32) {
        match self {
            Self::One(at, _) if *at == version => *self = Self::Empty,
            Self::Many(versions) => {
                if let Ok(index) = versions.binary_search_by_key(&version, |&(at, _)| at) {
                    versions.remove(index);
                }
            }
            _ => {}
        }
    }

    /// Returns the latest version below `version` and its value.
    fn latest_below(&self, version: u32) -> Option<(u32, &V)> {
        match self {
            Self::Empty => None,
            Self::One(at, value) => (*at < version).then_some((*at, value)),
            Self::Many(versions) => {
                let index = versions.partition_point(|&(at, _)| at < version).checked_sub(1)?;
                let (at, value) = &versions[index];
                Some((*at, value))
            }
        }
    }

    /// Returns the versions below `version` and their values, latest first.
    fn below(&self, version: u32) -> impl Iterator<Item = (u32, &V)> {
        let (one, many) = match self {
            Self::Empty => (None, &[][..]),
            Self::One(at, value) => ((*at < version).then_some((*at, value)), &[][..]),
            Self::Many(versions) => {
                (None, &versions[..versions.partition_point(|&(at, _)| at < version)])
            }
        };
        one.into_iter().chain(many.iter().rev().map(|(at, value)| (*at, value)))
    }
}

/// Sets the value at `version` in versions sorted by version.
fn insert_version<V>(versions: &mut Vec<(u32, V)>, version: u32, value: V) {
    match versions.binary_search_by_key(&version, |&(at, _)| at) {
        Ok(index) => versions[index].1 = value,
        Err(index) => versions.insert(index, (version, value)),
    }
}

/// A value aligned to its own cache lines, so threads using neighbouring values do not contend.
#[derive(Debug, Default)]
#[repr(align(128))]
struct CachePadded<T>(T);

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(test)]
mod tests;
