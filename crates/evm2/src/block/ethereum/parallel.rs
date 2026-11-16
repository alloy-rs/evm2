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
//! With [`ParallelCommit::Batched`], the in-order step does less: idle workers validate results
//! just ahead of it against the multi-version store and record how far its write log reached, so
//! the step only checks the keys written since, runs deferred storage operations against the
//! store and records the receipt. Blind adds stay deltas in the store, transactions that read stale
//! state execute again over it, and the block's final state reaches the executor once, at the end.
//! The state hook still streams: whenever [`STREAM_INTERVAL`] passed since its last update, the
//! committer sends it the latest value, read from the store, of every key the transactions
//! committed since wrote, so a state-root task hashes the block's prefix while the rest executes,
//! and a key many transactions write, such as a fee balance, is sent once per update rather than
//! once per transaction.
//! For blocks of cheap transactions, such as payments, the in-order step is the bound on
//! throughput.
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
    Evm, EvmFeatures, EvmTypes, TxResult, TxResultWithState, TxStateChanges,
    block::{
        AccountUpdate, BlockExecutionError, BlockExecutor, ExecutableTx, ReceiptBuilder,
        StateUpdate, bundle_state::StorageSlot, map_database_error,
    },
    bytecode::Bytecode,
    evm::{
        AccountFlags, AccountInfo, Cache, DbResult, DeferredBatchAccess, DeferredStorageOp,
        DeferredStorageStore, DynDatabase, DynDatabaseRef, PendingState,
    },
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
    any::{Any, TypeId},
    borrow::Borrow,
    cell::{Cell, RefCell, UnsafeCell},
    cmp::Reverse,
    hash::{BuildHasher, Hash, Hasher},
    num::NonZeroUsize,
    ops::Deref,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering, fence},
};
use shard_lock::{Readers, ShardLock, WriteGuard};
use std::{
    hash::RandomState,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Condvar, Mutex, OnceLock, RwLock, mpsc},
    thread::Thread,
    time::{Duration, Instant},
};

/// Number of lock shards of the multi-version account and storage maps.
const SHARDS: usize = 64;

/// Base-2 logarithm of the number of bits of each [`WriteFilter`] of a [`MvStore`].
const FILTER_BITS: u32 = 18;

/// Number of buckets of a [`Buckets`], each twice the size of the one before.
const BUCKETS: usize = 32;

/// Number of elements of the first bucket of a [`Buckets`].
const FIRST_BUCKET: usize = 64;

/// Times an idle worker polls for results to execute again before yielding its core.
const TAIL_SPINS: u32 = 1 << 6;

/// Times an idle worker yields its core and polls again before parking until woken.
const TAIL_YIELDS: u32 = 4;

/// Longest an idle worker parks before polling again, should no wake reach it.
const TAIL_PARK: core::time::Duration = core::time::Duration::from_millis(1);

/// How many commits the committer advances before waking a parked idle worker to validate ahead
/// of it.
const IDLE_WAKE_COMMITS: usize = 4;

/// How far ahead of the committer idle workers validate results in batched commits.
const PREVALIDATE_AHEAD: usize = 8;

/// Times the committer polls for the next result before parking.
const COMMIT_SPINS: u32 = 1 << 10;

/// How many committed transactions' buffers a batched commit hands back to a worker at once.
const RECYCLE_BATCH: usize = 16;

/// How far ahead of the committer idle workers run deferred storage operations to warm the slots
/// they read in batched commits.
const WARM_AHEAD: usize = 64;

/// Most threads that read a batched commit's final state.
const MATERIALIZE_THREADS: usize = 8;

/// How often a batched commit streams the state its committed prefix changed to the state hook.
/// Tests stream after every transaction.
const STREAM_INTERVAL: Duration =
    if cfg!(test) { Duration::ZERO } else { Duration::from_micros(200) };

/// How many transactions a batched commit commits between looks at the clock for
/// [`STREAM_INTERVAL`]. Tests look after every transaction.
const STREAM_STRIDE: usize = if cfg!(test) { 1 } else { 8 };

/// The identifier of the next [`MvStore`], which tells [`BASE_MEMO`] whose reads it holds.
static NEXT_STORE_ID: AtomicUsize = AtomicUsize::new(1);

std::thread_local! {
    /// The values the calling thread read from one store's base database. No transaction changes
    /// them, so every thread keeps its own copy instead of reading the base database, whose
    /// shared cache its other threads contend on, again.
    static BASE_MEMO: RefCell<BaseMemo> = RefCell::default();
}

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

/// How [`EthBlockExecutor::execute_transactions_parallel`] commits a block's transactions.
///
/// Both commit exactly the state, receipts and gas of sequential execution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParallelCommit {
    /// Commit each transaction to the executor's state in block order: its exact state update
    /// streams through the state hook as soon as its prefix of the block commits, and the
    /// block's dependencies are recorded as hints.
    #[default]
    InOrder,
    /// Validate and settle each transaction against the multi-version store alone, and commit the
    /// block's final state to the executor once, after the last transaction, as one state update.
    ///
    /// The in-order step then only checks reads, rebases balances, runs deferred storage
    /// operations and records the receipt: blind adds stay deltas in the store until the end, and
    /// transactions that read stale state execute again over the store rather than the executor's
    /// state. No dependency hints are recorded. An executor building a block access list, which
    /// records each transaction's changes, commits in order.
    Batched,
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
    /// Sets how [`Self::execute_transactions_parallel`] commits the block's transactions.
    pub const fn set_parallel_commit(&mut self, commit: ParallelCommit) {
        self.parallel_commit = commit;
    }

    /// Returns whether [`Self::execute_transactions_parallel`] commits in one batch: with
    /// [`ParallelCommit::Batched`] and no block access list builder, which needs in-order commit.
    pub const fn commits_batched(&self) -> bool {
        matches!(self.parallel_commit, ParallelCommit::Batched)
            && !self.block_access_list_builder_enabled()
    }

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
    /// - `on_commit` sees each transaction's final result, by index and in block order, before it
    ///   commits, and rejects the block by returning an error: for checks a wrapping executor runs
    ///   in [`BlockExecutor::commit_transaction`], such as block section or lane budgets.
    ///
    /// When [`Self::commits_batched`], a failure leaves the executor's receipts, gas counters and
    /// state as they were before the call, so the caller can execute the block again one
    /// transaction at a time, for the exact error sequential execution reports; batched commit then
    /// runs even on one thread or with serial hints. The state hook may by then have seen state the
    /// block's committed prefix changed, which executing it again streams again unchanged.
    /// Otherwise a failure leaves the transactions before it committed, and the executor must be
    /// discarded.
    ///
    /// Call it after [`BlockExecutor::apply_pre_execution_changes`] and before any transaction of
    /// the block. Do not attach a block access list for reads: a block with one executes in
    /// parallel from it directly (see the `bal_parallel` example).
    #[expect(clippy::too_many_arguments)]
    pub fn execute_transactions_parallel<'s, Tx, B, F, H, C>(
        &mut self,
        transactions: impl IntoIterator<Item = Tx>,
        base: &'s B,
        make_evm: F,
        threads: NonZeroUsize,
        hints: Option<&ParallelHints>,
        on_speculative: H,
        mut on_commit: C,
    ) -> Result<ParallelExecutionOutput, BlockExecutionError>
    where
        Tx: ExecutableTx<Self>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T> + Sync,
        H: Fn(usize, TxStateChanges<'_>) + Sync,
        C: FnMut(usize, &TxResult<T>) -> Result<(), BlockExecutionError>,
    {
        let (evm_txs, txs): (Vec<_>, Vec<_>) =
            transactions.into_iter().map(ExecutableTx::into_parts).unzip();
        let mut stats = ParallelExecutionStats { transactions: txs.len(), ..Default::default() };
        let mut dependencies = DependencyTracker::new(self.evm.block_env().beneficiary);
        let batched = self.commits_batched();
        let serial = hints.is_some_and(|hints| hints.critical_path() * 2 > txs.len());
        if txs.is_empty() || (!batched && (threads.get() == 1 || txs.len() < 2 || serial)) {
            for (index, (evm_tx, tx)) in evm_txs.into_iter().zip(txs).enumerate() {
                let output = self.execute_transaction_without_commit((evm_tx, tx))?;
                on_commit(index, &output.result().result)?;
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
        let workers = threads.get().min(txs.len());
        let store = MvStore::new(self.evm.overlay_db().cache.clone(), base, workers);
        // This thread commits.
        store.register_reader(workers);
        let results = Results::new(workers, batched);
        let totals = batched.then(|| self.totals());
        let output = std::thread::scope(|scope| {
            for worker in 0..workers {
                let (store, scheduler, queue, results) = (&store, &scheduler, &queue, &results);
                let (make_evm, on_speculative) = (&make_evm, &on_speculative);
                scope.spawn(move || {
                    run_worker(
                        store,
                        scheduler,
                        queue,
                        make_evm,
                        on_speculative,
                        &|_: &T::Tx, _: &TxResult<T>| None::<()>,
                        results,
                        worker,
                    );
                });
            }
            let result = if batched {
                self.commit_batched(
                    &store,
                    &queue,
                    txs,
                    &results,
                    &mut stats,
                    &make_evm,
                    &mut on_commit,
                    &mut |_, _, _| {},
                )
            } else {
                self.commit_in_order(
                    &store,
                    &queue,
                    txs,
                    &results,
                    &mut stats,
                    &mut dependencies,
                    &mut on_commit,
                    &mut |_, _, _| {},
                )
            };
            scheduler.abort();
            results.wake_all_idle();
            result
        });
        store.drop_in_background();
        results.drop_buffers_in_background();
        if let (Err(_), Some(totals)) = (&output, totals) {
            // Nothing was committed: forget the receipts and gas of the transactions before the
            // failure.
            self.restore_totals(totals);
        }
        output?;
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
    /// `on_commit` works as in [`Self::execute_transactions_parallel`], and `on_receipt` sees each
    /// transaction's receipt, by index and in block order, as soon as it is recorded, so a
    /// receipt-root task can hash the block's receipts while later transactions still execute.
    /// `copy_receipt` runs on a worker after each speculative execution, with its transaction and
    /// result, and returns a copy of the receipt the result records, if it prepares one:
    /// `on_receipt` gets the copy prepared from the result it records, and `None` for a result
    /// without one, such as a transaction the committer executed again. The copy cannot know the
    /// block's cumulative gas, which `on_receipt` reads from the recorded receipt.
    /// When [`Self::commits_batched`], the commit is batched as in
    /// [`Self::execute_transactions_parallel`], with the same guarantees on failure: the executor
    /// then forgets the receipts `on_receipt` saw, and executing the block again one transaction at
    /// a time records the same receipts for the transactions before the failure.
    ///
    /// When the block fails to execute, no new transaction starts, but the call returns only once
    /// `transactions` yields its next item or ends.
    #[expect(clippy::too_many_arguments)]
    pub fn execute_transaction_stream<'s, Tx, I, B, F, H, C, K, Q, P>(
        &mut self,
        transactions: I,
        base: &'s B,
        make_evm: F,
        threads: NonZeroUsize,
        on_speculative: H,
        mut on_commit: C,
        copy_receipt: K,
        mut on_receipt: P,
    ) -> Result<ParallelExecutionOutput, BlockExecutionError>
    where
        Tx: ExecutableTx<Self, Tx: Send>,
        I: IntoIterator<Item = StreamedTx<Tx>, IntoIter: Send>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T> + Sync,
        H: Fn(usize, TxStateChanges<'_>) + Sync,
        C: FnMut(usize, &TxResult<T>) -> Result<(), BlockExecutionError>,
        K: Fn(&T::Tx, &TxResult<T>) -> Option<Q> + Sync,
        Q: Send,
        P: FnMut(usize, &R::Receipt, Option<Q>),
    {
        let mut stats = ParallelExecutionStats::default();
        let mut dependencies = DependencyTracker::new(self.evm.block_env().beneficiary);
        let batched = self.commits_batched();
        if threads.get() == 1 && !batched {
            for (index, streamed) in transactions.into_iter().enumerate() {
                let output = self.execute_transaction_without_commit(streamed.tx)?;
                on_commit(index, &output.result().result)?;
                dependencies.record(index, &output.result().pending_state);
                self.commit_transaction(output)?;
                self.emit_receipt(index, &mut on_receipt, None);
                stats.transactions += 1;
                stats.reexecuted += 1;
            }
            return Ok(ParallelExecutionOutput { stats, dependencies: dependencies.finish() });
        }

        let scheduler = Scheduler::default();
        let queue = TxQueue::default();
        let store = MvStore::new(self.evm.overlay_db().cache.clone(), base, threads.get());
        // This thread commits.
        store.register_reader(threads.get());
        let results = Results::new(threads.get(), batched);
        let totals = batched.then(|| self.totals());
        let (tx_sender, tx_receiver) = mpsc::channel();
        let transactions = transactions.into_iter();
        let output = std::thread::scope(|scope| {
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
                let (make_evm, on_speculative, copy_receipt) =
                    (&make_evm, &on_speculative, &copy_receipt);
                scope.spawn(move || {
                    run_worker(
                        store,
                        scheduler,
                        queue,
                        make_evm,
                        on_speculative,
                        copy_receipt,
                        results,
                        worker,
                    );
                });
            }
            let result = if batched {
                self.commit_batched(
                    &store,
                    queue,
                    tx_receiver,
                    &results,
                    &mut stats,
                    &make_evm,
                    &mut on_commit,
                    &mut on_receipt,
                )
            } else {
                self.commit_in_order(
                    &store,
                    queue,
                    tx_receiver,
                    &results,
                    &mut stats,
                    &mut dependencies,
                    &mut on_commit,
                    &mut on_receipt,
                )
            };
            scheduler.abort();
            results.wake_all_idle();
            result
        });
        store.drop_in_background();
        results.drop_buffers_in_background();
        if let (Err(_), Some(totals)) = (&output, totals) {
            // Nothing was committed: forget the receipts and gas of the transactions before the
            // failure.
            self.restore_totals(totals);
        }
        output?;
        Ok(ParallelExecutionOutput { stats, dependencies: dependencies.finish() })
    }

    /// Hands the receipt the transaction at `index` just recorded to `on_receipt`, with the copy
    /// of it a worker prepared, if any.
    fn emit_receipt<Q>(
        &self,
        index: usize,
        on_receipt: &mut impl FnMut(usize, &R::Receipt, Option<Q>),
        copy: Option<Q>,
    ) {
        if let Some(receipt) = self.receipts.last() {
            on_receipt(index, receipt, copy);
        }
    }

    /// Validates and commits speculative executions in block order, executing invalid ones again.
    ///
    /// `txs` yields the consensus transactions in block order; the transaction at an index is in
    /// `queue` before `txs` yields it.
    #[expect(clippy::too_many_arguments)]
    fn commit_in_order<X: Borrow<R::Transaction>, Q>(
        &mut self,
        store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
        queue: &TxQueue<T::Tx>,
        txs: impl IntoIterator<Item = X>,
        results: &Results<Speculative<T, Q>>,
        stats: &mut ParallelExecutionStats,
        dependencies: &mut DependencyTracker,
        on_commit: &mut impl FnMut(usize, &TxResult<T>) -> Result<(), BlockExecutionError>,
        on_receipt: &mut impl FnMut(usize, &R::Receipt, Option<Q>),
    ) -> Result<(), BlockExecutionError> {
        for (index, tx) in txs.into_iter().enumerate() {
            stats.transactions = index + 1;
            let Speculative { worker, outcome, reward, mut reads, mut writes, receipt, .. } =
                results.take(index).expect("parallel execution workers exited without a result");
            let evm_tx = queue.get(index);
            let tx = tx.borrow();
            let version = version(index);

            self.set_transaction_block_access_index();
            self.validate_transaction_gas_limit(tx.gas_limit())?;
            let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
            self.validate_transaction_blob_gas(blob_gas_used)?;

            let (pending, receipt) =
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
                        if pending_state
                            .deferred()
                            .is_some_and(|deferred| !deferred.resolved.is_empty())
                        {
                            store.publish_resolved(version, &pending_state);
                        }
                        on_commit(index, &result)?;
                        dependencies.record(index, &pending_state);
                        self.commit_pending_state(&pending_state);
                        self.record_receipt(tx.tx_type(), blob_gas_used, result);
                        (pending_state, receipt)
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
                        results.wake_idle();
                        let committed = committed.into_iter().collect::<HashSet<_>>();
                        store.unpublish(
                            version,
                            writes.iter().filter(|write| !committed.contains(*write)),
                        );
                        on_commit(index, &result.result)?;
                        dependencies.record(index, &result.pending_state);
                        self.commit_transaction(EthTransactionResultWithState::new(
                            result,
                            tx.tx_type(),
                            blob_gas_used,
                        ))?;
                        (PendingState::default(), None)
                    }
                };
            self.emit_receipt(index, on_receipt, receipt);
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
        if !rebase_balances(&mut outcome.pending_state, &reads.rebase) {
            return None;
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
            || reads.accounts.iter().any(|(address, ..)| *address == beneficiary)
        {
            return None;
        }
        // Credit the reward as the default settlement would have: load, touch, add.
        let original = self.evm.read_account_info(&beneficiary).ok()?;
        let current =
            credit_reward(original.as_ref(), reward, self.evm.feature(EvmFeatures::EIP161))?;
        outcome.pending_state.insert_account(beneficiary, original, current);
        if let Some(account) = outcome.pending_state.accounts.get_mut(&beneficiary) {
            account.flags.insert(AccountFlags::TOUCHED);
        }
        Some(outcome)
    }

    /// Validates and settles speculative executions in block order against `store` alone,
    /// executing invalid ones again over it, then commits the block's final state to the executor
    /// at once. See [`ParallelCommit::Batched`].
    ///
    /// The state hook runs on a streamer thread, which reads the values the committed prefix
    /// wrote from the store, so the committer only hands it the keys.
    #[expect(clippy::too_many_arguments)]
    fn commit_batched<X, B, F, Q>(
        &mut self,
        store: &MvStore<'_, B>,
        queue: &TxQueue<T::Tx>,
        txs: impl IntoIterator<Item = X>,
        results: &Results<Speculative<T, Q>>,
        stats: &mut ParallelExecutionStats,
        make_evm: &F,
        on_commit: &mut impl FnMut(usize, &TxResult<T>) -> Result<(), BlockExecutionError>,
        on_receipt: &mut impl FnMut(usize, &R::Receipt, Option<Q>),
    ) -> Result<(), BlockExecutionError>
    where
        X: Borrow<R::Transaction>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T>,
    {
        let Some(hook) = self.state_update_hook.take() else {
            let lifecycle = self.commit_batched_with(
                store, queue, txs, results, stats, make_evm, on_commit, on_receipt, None,
            )?;
            let mut state = store.materialize().map_err(map_database_error)?;
            store.apply_lifecycle(&mut state, &lifecycle).map_err(map_database_error)?;
            self.commit_block_changes(state);
            return Ok(());
        };
        std::thread::scope(|scope| {
            let (sender, batches) = mpsc::channel();
            let streamer = scope.spawn(move || stream_updates(store, &batches, hook));
            let committed = self.commit_batched_with(
                store,
                queue,
                txs,
                results,
                stats,
                make_evm,
                on_commit,
                on_receipt,
                Some(&sender),
            );
            drop(sender);
            let (hook, streamed) = match streamer.join() {
                Ok(joined) => joined,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            // The streamer built the block's changes as it streamed them: commit them before the
            // hook is back, which already received every change.
            let committed = committed.and_then(|lifecycle| {
                let mut state = streamed.map_err(map_database_error)?;
                store.apply_lifecycle(&mut state, &lifecycle).map_err(map_database_error)?;
                self.commit_block_changes(state);
                Ok(())
            });
            self.state_update_hook = Some(hook);
            committed
        })
    }

    /// Validates and settles the block's transactions as [`Self::commit_batched`], sending the
    /// keys each committed prefix wrote to `streamer` when the commit streams state, and returns
    /// the accounts the transactions created or whose storage they wiped. The block's final state
    /// is then in `store`, for the caller to commit.
    #[expect(clippy::too_many_arguments)]
    fn commit_batched_with<X, B, F, Q>(
        &mut self,
        store: &MvStore<'_, B>,
        queue: &TxQueue<T::Tx>,
        txs: impl IntoIterator<Item = X>,
        results: &Results<Speculative<T, Q>>,
        stats: &mut ParallelExecutionStats,
        make_evm: &F,
        on_commit: &mut impl FnMut(usize, &TxResult<T>) -> Result<(), BlockExecutionError>,
        on_receipt: &mut impl FnMut(usize, &R::Receipt, Option<Q>),
        streamer: Option<&mpsc::Sender<StreamBatch>>,
    ) -> Result<AddressMap<Lifecycle>, BlockExecutionError>
    where
        X: Borrow<R::Transaction>,
        B: DynDatabaseRef + Sync + ?Sized,
        F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T>,
    {
        let beneficiary = self.evm.block_env().beneficiary;
        let fee_charge = self.evm.feature(EvmFeatures::FEE_CHARGE);
        let eip161 = self.evm.feature(EvmFeatures::EIP161);
        // The beneficiary with the rewards credited since it was last published to the store.
        let mut rewarded = None;
        let view_version = Cell::new(0);
        let view_reads = RefCell::new(ReadSet::default());
        let mut evm = None;
        let mut last_version = 0;
        // Accounts created or whose storage was wiped, as their transactions committed them.
        let mut lifecycle = AddressMap::<Lifecycle>::default();
        let mut stream = streamer.is_some().then(PrefixStream::default);
        // Each worker's committed buffers, handed back in batches.
        let mut spare = results.pools.iter().map(|_| Vec::new()).collect::<Vec<_>>();
        // Batched deferred storage operations of the committed transactions, by type.
        let mut batches = Vec::<(TypeId, Vec<DeferredStorageOp>)>::new();
        for (index, tx) in txs.into_iter().enumerate() {
            stats.transactions = index + 1;
            let Speculative {
                worker,
                outcome,
                reward,
                mut reads,
                writes,
                prevalidated,
                lifecycle: created_or_wiped,
                receipt,
            } = results.take(index).expect("parallel execution workers exited without a result");
            let evm_tx = queue.get(index);
            let tx = tx.borrow();
            let version = version(index);
            last_version = version;
            // A worker's validation holds unless a key it read was written below since.
            let validated = prevalidated
                .filter(|&(mark, _)| !store.logged_since(mark, version, &reads))
                .map(|(_, valid)| valid);

            self.set_transaction_block_access_index();
            self.validate_transaction_gas_limit(tx.gas_limit())?;
            let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
            self.validate_transaction_blob_gas(blob_gas_used)?;

            let settled = outcome.and_then(|outcome| {
                settle_batched(
                    outcome,
                    validated,
                    &mut reads,
                    &evm_tx.signer(),
                    store,
                    version,
                    fee_charge.then_some((beneficiary, reward, eip161, &mut rewarded)),
                )
            });
            let (pending, receipt) = match settled {
                Some(result) => {
                    stats.speculative += 1;
                    let TxResultWithState { result, mut pending_state, .. } = result;
                    for (address, ..) in &reads.rebase {
                        if let Some(account) = pending_state.accounts.get(address)
                            && account.is_changed()
                        {
                            store.republish_account(version, *address, account.present.as_ref());
                        }
                    }
                    run_deferred_ops(
                        store,
                        version,
                        &mut pending_state,
                        stream.as_mut(),
                        &mut batches,
                    )?;
                    on_commit(index, &result)?;
                    // Settling and deferred operations create no account and wipe no storage.
                    if created_or_wiped {
                        record_lifecycle(&mut lifecycle, &pending_state);
                    }
                    self.record_receipt(tx.tx_type(), blob_gas_used, result);
                    if let Some(stream) = &mut stream {
                        stream.keys.extend(writes.iter().copied());
                        if fee_charge && !reward.is_zero() {
                            stream.keys.push(WriteKey::Account(beneficiary));
                        }
                    }
                    (pending_state, receipt)
                }
                None => {
                    stats.reexecuted += 1;
                    // It may read the beneficiary: publish the rewards credited since.
                    if let Some(info) = rewarded.take() {
                        store.republish_account(version - 1, beneficiary, info.as_ref());
                        if let Some(stream) = &mut stream {
                            stream.keys.push(WriteKey::Account(beneficiary));
                        }
                    }
                    let evm = evm.get_or_insert_with(|| {
                        let view = TxView { store, version: &view_version, reads: &view_reads };
                        let mut evm = make_evm(Box::new(view));
                        evm.set_defer_storage_resolution(true);
                        evm
                    });
                    // Every earlier transaction committed to the store: it now holds the state
                    // sequential execution reaches.
                    let cache = &mut evm.overlay_db_mut().cache;
                    cache.accounts.clear();
                    cache.storage.clear();
                    view_version.set(version);
                    evm.state_mut().clear_transaction_state();
                    let result = execute_transaction_without_commit(evm, evm_tx)
                        .map_err(|error| map_transaction_error(error, *tx.tx_hash()))?;
                    view_reads.borrow_mut().clear();
                    let TxResultWithState { result, mut pending_state, .. } = result;
                    let mut committed = Vec::new();
                    store.publish(version, &pending_state, &mut committed);
                    results.revalidate.fetch_min(index + 1, Ordering::AcqRel);
                    results.wake_idle();
                    let committed = committed.into_iter().collect::<HashSet<_>>();
                    store.unpublish(
                        version,
                        writes.iter().filter(|write| !committed.contains(*write)),
                    );
                    if let Some(stream) = &mut stream {
                        stream.keys.extend(committed.iter().copied());
                    }
                    store.log_writes(version, committed.into_iter().chain(writes.iter().copied()));
                    run_deferred_ops(
                        store,
                        version,
                        &mut pending_state,
                        stream.as_mut(),
                        &mut batches,
                    )?;
                    on_commit(index, &result)?;
                    record_lifecycle(&mut lifecycle, &pending_state);
                    self.record_receipt(tx.tx_type(), blob_gas_used, result);
                    (PendingState::default(), None)
                }
            };
            self.emit_receipt(index, on_receipt, receipt);
            // The worker empties the buffers when it takes them back.
            spare[worker].push(Buffers { reads, writes, pending });
            if spare[worker].len() >= RECYCLE_BATCH {
                results.recycle_all(worker, &mut spare[worker]);
            }
            if let (Some(stream), Some(streamer)) = (&mut stream, streamer)
                && index % STREAM_STRIDE == 0
                && stream.last.elapsed() >= STREAM_INTERVAL
                && !stream.keys.is_empty()
            {
                stream.last = Instant::now();
                let running = rewarded.as_ref().map(|info| (beneficiary, info.clone()));
                let capacity = stream.keys.capacity();
                let keys = core::mem::replace(&mut stream.keys, Vec::with_capacity(capacity));
                // The streamer only stops early on an error, which it reports when joined.
                let _ = streamer.send(StreamBatch { keys, version: version + 1, running });
            }
        }
        for (worker, buffers) in spare.iter_mut().enumerate() {
            results.recycle_all(worker, buffers);
        }
        if let Some(info) = rewarded.take() {
            store.republish_account(last_version, beneficiary, info.as_ref());
        }
        for (_, ops) in &batches {
            let mut access = DeferredBatchAccess::default();
            ops[0]
                .as_batched()
                .expect("batches hold batched operations")
                .run_batch(ops, &mut access);
            let mut writes = access.into_writes();
            if let Some(stream) = &mut stream {
                stream.keys.extend(
                    writes.iter().map(|&(address, key, _)| WriteKey::Storage(address, key)),
                );
            }
            store.put_slots(last_version, &mut writes);
        }
        if let (Some(stream), Some(streamer)) = (&mut stream, streamer) {
            let keys = core::mem::take(&mut stream.keys);
            let _ = streamer.send(StreamBatch { keys, version: last_version + 1, running: None });
        }
        Ok(lifecycle)
    }

    /// Commits a batched commit's block changes to the block state and the EVM overlay, one on a
    /// helper thread, without streaming them.
    fn commit_block_changes(&mut self, state: PendingState) {
        let (evm, block_state) = (&mut self.evm, &mut self.block_state);
        std::thread::scope(|scope| {
            let state = &state;
            scope.spawn(move || {
                for changes in state.account_changes() {
                    block_state.commit_account_changes(changes, None);
                }
            });
            evm.overlay_db_mut().commit_pending(state);
        });
        drop_in_background(state);
    }

    /// Returns the receipt count and gas counters, to restore with [`Self::restore_totals`].
    const fn totals(&self) -> Totals {
        Totals {
            receipts: self.receipts.len(),
            cumulative_gas_used: self.cumulative_gas_used,
            block_regular_gas_used: self.block_regular_gas_used,
            block_state_gas_used: self.block_state_gas_used,
            blob_gas_used: self.blob_gas_used,
        }
    }

    /// Drops the receipts recorded since `totals` and restores the gas counters.
    fn restore_totals(&mut self, totals: Totals) {
        self.receipts.truncate(totals.receipts);
        self.cumulative_gas_used = totals.cumulative_gas_used;
        self.block_regular_gas_used = totals.block_regular_gas_used;
        self.block_state_gas_used = totals.block_state_gas_used;
        self.blob_gas_used = totals.blob_gas_used;
    }
}

/// The receipt count and gas counters of an executor, from before a batched commit.
#[derive(Clone, Copy, Debug)]
struct Totals {
    receipts: usize,
    cumulative_gas_used: u64,
    block_regular_gas_used: u64,
    block_state_gas_used: u64,
    blob_gas_used: u64,
}

/// Whether a block's committed transactions created an account or wiped its storage.
#[derive(Clone, Copy, Debug, Default)]
struct Lifecycle {
    created: bool,
    wiped: bool,
}

/// Returns whether a transaction created an account or wiped storage: whether
/// [`record_lifecycle`] may record any of its accounts.
fn has_lifecycle(pending: &PendingState) -> bool {
    pending.accounts.values().any(|account| account.is_created() || account.storage.wiped)
}

/// Records the accounts a committed transaction created or whose storage it wiped.
fn record_lifecycle(lifecycle: &mut AddressMap<Lifecycle>, pending: &PendingState) {
    for changes in pending.account_changes() {
        let wiped = changes.storage.wiped;
        if (changes.created || wiped) && changes.is_changed() {
            let entry = lifecycle.entry(changes.address).or_default();
            entry.created |= changes.created;
            entry.wiped |= wiped;
        }
    }
}

/// Validates a speculative execution against `store` at `version`, where every earlier
/// transaction committed, unless `validated` holds a worker's still valid verdict, rebases the
/// balances it did not read exactly, and, when fees are charged, credits the withheld reward to
/// the running `beneficiary` account. Returns `None` when it must execute again.
fn settle_batched<T: EvmTypes, B: DynDatabaseRef + Sync + ?Sized>(
    mut outcome: TxResultWithState<T>,
    validated: Option<bool>,
    reads: &mut ReadSet,
    sender: &Address,
    store: &MvStore<'_, B>,
    version: u32,
    beneficiary: Option<(Address, Word, bool, &mut Option<Option<AccountInfo>>)>,
) -> Option<TxResultWithState<T>> {
    let valid = match validated {
        Some(valid) => valid,
        None => reads.validate_store(store, version, &outcome.pending_state, sender),
    };
    if !valid {
        return None;
    }
    if !rebase_balances(&mut outcome.pending_state, &reads.rebase) {
        return None;
    }
    let Some((beneficiary, reward, eip161, rewarded)) = beneficiary else { return Some(outcome) };
    // The transaction read or wrote the beneficiary itself: execute it again over the store.
    if outcome.pending_state.accounts.contains_key(&beneficiary)
        || reads.accounts.iter().any(|(address, ..)| *address == beneficiary)
    {
        return None;
    }
    let current = match rewarded {
        // Crediting nothing to an account that exists and is not empty leaves it as it is.
        Some(Some(info)) if reward.is_zero() && !info.is_empty() => return Some(outcome),
        Some(info) => credit_reward(info.as_ref(), reward, eip161)?,
        None => credit_reward(store.account(&beneficiary, version).ok()?.as_ref(), reward, eip161)?,
    };
    *rewarded = Some(current);
    Some(outcome)
}

/// Applies the balance changes `pending` made without reading the balances exactly over the
/// balances earlier transactions left: `rebase` holds each account's balance read and the one
/// committed. Returns `false` when the transaction must execute again.
fn rebase_balances(pending: &mut PendingState, rebase: &[(Address, Word, Word)]) -> bool {
    for &(address, read, now) in rebase {
        let Some(account) = pending.accounts.get_mut(&address) else { continue };
        let (Some(original), Some(present)) = (&mut account.original, &mut account.present) else {
            return false;
        };
        if original.balance != read {
            return false;
        }
        original.balance = now;
        present.balance = present.balance.wrapping_sub(read).wrapping_add(now);
        // Emptiness decides EIP-161 deletion, which the execution settled on its balances.
        if present.is_empty() {
            return false;
        }
    }
    true
}

/// Returns the beneficiary `original` after the default settlement credits `reward` to it:
/// load, touch, add. Returns `None` when the sequential EVM must order the touch.
fn credit_reward(
    original: Option<&AccountInfo>,
    reward: Word,
    eip161: bool,
) -> Option<Option<AccountInfo>> {
    match original {
        Some(info) if reward.is_zero() => {
            // Touching an existing empty account deletes it under EIP-161.
            if info.is_empty() && eip161 {
                return None;
            }
            Some(Some(info.clone()))
        }
        // Before EIP-161, touching an absent account creates it empty: let the sequential EVM do
        // it.
        None if reward.is_zero() && !eip161 => None,
        None if reward.is_zero() => Some(None),
        Some(info) => {
            let mut info = info.clone();
            info.balance = info.balance.wrapping_add(reward);
            // A reward that wraps the balance to zero leaves an empty account, which EIP-161
            // deletes.
            if info.is_empty() && eip161 {
                return None;
            }
            Some(Some(info))
        }
        None => Some(Some(AccountInfo { balance: reward, ..AccountInfo::empty() })),
    }
}

/// Runs a committed transaction's deferred storage operations against `store`, in order, at the
/// transaction's `version`, and adds its batched ones to `batches`, to run at the block's end.
fn run_deferred_ops<B: DynDatabaseRef + Sync + ?Sized>(
    store: &MvStore<'_, B>,
    version: u32,
    pending: &mut PendingState,
    stream: Option<&mut PrefixStream>,
    batches: &mut Vec<(TypeId, Vec<DeferredStorageOp>)>,
) -> Result<(), BlockExecutionError> {
    let Some(parallel) =
        pending.parallel.as_deref_mut().filter(|parallel| !parallel.deferred.ops.is_empty())
    else {
        return Ok(());
    };
    let mut access = StoreAccess { store, version, stream, written: Vec::new() };
    let ran = run_ops_on(&mut access, &mut parallel.deferred.ops, batches);
    // Only the committer checks the log, after this returns: one append covers every slot written
    // above.
    if !access.written.is_empty() {
        store.log_writes(version, access.written);
    }
    ran
}

/// Runs `ops` through `access` as [`run_deferred_ops`] does, without logging the slots written.
fn run_ops_on<B: DynDatabaseRef + Sync + ?Sized>(
    access: &mut StoreAccess<'_, '_, B>,
    ops: &mut Vec<DeferredStorageOp>,
    batches: &mut Vec<(TypeId, Vec<DeferredStorageOp>)>,
) -> Result<(), BlockExecutionError> {
    for op in core::mem::take(ops) {
        let Some(batched) = op.as_batched() else {
            op.run_on(access).map_err(map_database_error)?;
            continue;
        };
        let type_id = (batched as &dyn Any).type_id();
        let ops = match batches.iter().position(|(id, _)| *id == type_id) {
            Some(index) => &mut batches[index].1,
            None => {
                batches.push((type_id, Vec::new()));
                &mut batches.last_mut().expect("pushed above").1
            }
        };
        let mut writes = DeferredBatchAccess::default();
        batched.commit(ops, &mut writes).map_err(map_database_error)?;
        let mut writes = writes.into_writes();
        if let Some(stream) = &mut access.stream {
            stream
                .keys
                .extend(writes.iter().map(|&(address, key, _)| WriteKey::Storage(address, key)));
        }
        access.store.put_slots(access.version, &mut writes);
        access
            .written
            .extend(writes.iter().map(|&(address, key, _)| WriteKey::Storage(address, key)));
        ops.push(op);
    }
    Ok(())
}

/// A deferred storage operation's view of the store: the state its transaction leaves, written
/// at the transaction's version.
struct StoreAccess<'m, 's, B: ?Sized> {
    store: &'m MvStore<'s, B>,
    version: u32,
    /// Records the slots written, when the commit streams state.
    stream: Option<&'m mut PrefixStream>,
    /// The slots written, to log once the operations ran.
    written: Vec<WriteKey>,
}

impl<B: DynDatabaseRef + Sync + ?Sized> DeferredStorageStore for StoreAccess<'_, '_, B> {
    fn sload(&mut self, address: &Address, key: Word) -> DbResult<Word> {
        self.store.storage(address, &key, self.version + 1)
    }

    fn sstore(&mut self, address: &Address, key: Word, value: Word) -> DbResult<()> {
        self.store.put_slot(self.version, *address, key, value);
        self.written.push(WriteKey::Storage(*address, key));
        if let Some(stream) = &mut self.stream {
            stream.keys.push(WriteKey::Storage(*address, key));
        }
        Ok(())
    }
}

/// Returns the index of the multi-version store shard holding the slot `key` of `address`.
#[inline]
fn storage_shard_index(address: &Address, key: &Word) -> usize {
    (address[19] as usize ^ key.as_limbs()[0] as usize) % SHARDS
}

/// Returns the multi-version store version holding the writes of the transaction at `index`.
/// Version 0 is the state before the first transaction.
#[inline]
fn version(index: usize) -> u32 {
    u32::try_from(index + 1).expect("block has fewer than 2^32 transactions")
}

/// The state a batched commit has yet to stream to the state hook, and what it streamed.
struct PrefixStream {
    /// Keys the transactions committed since the last update wrote, with repeats.
    keys: Vec<WriteKey>,
    /// Each account and slot streamed, from its value before the block to its value as last
    /// streamed: once the last update is taken, the block's changes.
    state: PendingState,
    /// Accounts streamed with code they did not have before the block, to load it into
    /// `state` at the end.
    code: Vec<Address>,
    /// When the last update was taken.
    last: Instant,
}

impl Default for PrefixStream {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            state: PendingState::default(),
            code: Vec::new(),
            last: Instant::now(),
        }
    }
}

impl PrefixStream {
    /// Takes the update that brings the hook from the state last streamed to the store's state
    /// at `version`, where every earlier transaction committed: each account and slot written
    /// since, once, at its value there. `running` is the beneficiary's latest value when the store
    /// does not hold it yet.
    fn take<B: DynDatabaseRef + Sync + ?Sized>(
        &mut self,
        store: &MvStore<'_, B>,
        version: u32,
        running: Option<(Address, Option<&AccountInfo>)>,
    ) -> DbResult<StateUpdate> {
        self.last = Instant::now();
        let mut written = AddressMap::<(bool, Vec<Word>)>::default();
        for key in self.keys.drain(..) {
            match key {
                WriteKey::Account(address) | WriteKey::Wipe(address) => {
                    written.entry(address).or_default().0 = true;
                }
                WriteKey::Storage(address, key) => written.entry(address).or_default().1.push(key),
            }
        }
        let mut update = StateUpdate { accounts: Vec::with_capacity(written.len()) };
        for (address, (account_written, mut keys)) in written {
            let current = match running {
                Some((beneficiary, info)) if beneficiary == address => info.cloned(),
                _ => store.account(&address, version)?,
            };
            let original = match self.state.accounts.get_mut(&address) {
                Some(account) => {
                    let original = account.present.clone();
                    if account_written {
                        account.present = current.clone();
                    }
                    original
                }
                None => {
                    let original = match account_written {
                        true => store.account_initial(&address)?,
                        false => current.clone(),
                    };
                    self.state.insert_account(address, original.clone(), current.clone());
                    original
                }
            };
            if account_written
                && let Some(info) = &current
                && info.code_hash != KECCAK256_EMPTY
                && !info.code_hash.is_zero()
            {
                self.code.push(address);
            }
            if keys.is_empty() && original == current {
                continue;
            }
            keys.sort_unstable();
            keys.dedup();
            let mut storage = Vec::with_capacity(keys.len());
            for key in keys {
                let value = store.storage(&address, &key, version)?;
                let slots = &mut self
                    .state
                    .accounts
                    .get_mut(&address)
                    .expect("inserted above")
                    .storage
                    .slots;
                let previous = match slots.get_mut(&key) {
                    Some(slot) => core::mem::replace(&mut slot.value.current, value),
                    None => {
                        let original = store.storage_initial(&address, &key)?;
                        self.state.insert_storage(address, key, original, value);
                        original
                    }
                };
                storage.push((key, StorageSlot::new_changed(previous, value)));
            }
            update.accounts.push(AccountUpdate {
                address,
                original,
                current,
                created: false,
                wiped: false,
                storage,
            });
        }
        Ok(update)
    }

    /// Returns the block's changes once the last update was taken: each account and slot written,
    /// from its value before the block to its latest value, with the code of each account whose
    /// code changed.
    fn finish<B: DynDatabaseRef + Sync + ?Sized>(
        mut self,
        store: &MvStore<'_, B>,
    ) -> DbResult<PendingState> {
        for address in self.code {
            if let Some(account) = self.state.accounts.get_mut(&address)
                && let Some(info) = &mut account.present
                && info.code.is_none()
                && account
                    .original
                    .as_ref()
                    .is_none_or(|original| original.code_hash != info.code_hash)
            {
                info.code = Some(store.code(&info.code_hash)?);
            }
        }
        Ok(self.state)
    }
}

/// The keys a batched commit's prefix wrote since its last batch, for the streamer to read at
/// `version`, where every earlier transaction committed.
struct StreamBatch {
    keys: Vec<WriteKey>,
    version: u32,
    /// The beneficiary's latest value when the store does not hold it yet.
    running: Option<(Address, Option<AccountInfo>)>,
}

/// Streams the state each batch of committed keys reached to `hook`, in order, coalescing the
/// batches that queued while the previous update was taken. Returns the hook and the block's
/// changes, or the first error, after which it only drains the batches.
fn stream_updates<B: DynDatabaseRef + Sync + ?Sized>(
    store: &MvStore<'_, B>,
    batches: &mpsc::Receiver<StreamBatch>,
    mut hook: Box<dyn FnMut(StateUpdate) + Send>,
) -> (Box<dyn FnMut(StateUpdate) + Send>, DbResult<PendingState>) {
    let mut stream = PrefixStream::default();
    let mut result = Ok(());
    while let Ok(mut batch) = batches.recv() {
        if result.is_err() {
            continue;
        }
        stream.keys.append(&mut batch.keys);
        while let Ok(mut next) = batches.try_recv() {
            stream.keys.append(&mut next.keys);
            batch = next;
        }
        let running = batch.running.as_ref().map(|(address, info)| (*address, info.as_ref()));
        match stream.take(store, batch.version, running) {
            Ok(update) if !update.is_empty() => hook(update),
            Ok(_) => {}
            Err(error) => result = Err(error),
        }
    }
    (hook, result.and_then(|()| stream.finish(store)))
}

/// Drops `value` on a background thread, off the critical path, or here when no thread spawns.
fn drop_in_background<V: Send + 'static>(value: V) {
    let _ = std::thread::Builder::new().name("evm2-drop".into()).spawn(move || drop(value));
}

/// The deferred storage operations an idle worker last ran ahead of the committer, to read the
/// slots the committer will read into the base database's caches.
#[derive(Debug, Default)]
struct DeferredWarmer {
    /// Index of the next result whose operations to run.
    next: usize,
    /// The slots the operations run so far wrote.
    slots: SlotMap<Word>,
}

/// Runs the deferred storage operations of the next result ahead of the committer against the
/// store and the writes of the operations run before it, discarding their writes, so the slots
/// they read are loaded before the committer runs them. Returns whether it ran a result's.
fn warm_deferred<T: EvmTypes, Q>(
    store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
    results: &Results<Speculative<T, Q>>,
) -> bool {
    let Ok(mut warmer) = results.warmer.try_lock() else { return false };
    let committing = results.committing.load(Ordering::Acquire);
    if warmer.next <= committing {
        warmer.next = committing + 1;
        warmer.slots.clear();
    }
    let index = warmer.next;
    if index >= results.ready_prefix() || index > committing + WARM_AHEAD {
        return false;
    }
    let mut ops = None;
    if !results.update(index, |speculative| {
        ops = speculative
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.pending_state.deferred())
            .map(|deferred| deferred.ops.clone());
    }) {
        return false;
    }
    warmer.next = index + 1;
    let Some(ops) = ops else { return true };
    let warmer = &mut *warmer;
    let mut access = WarmAccess { store, version: version(index), slots: &mut warmer.slots };
    // Batched operations run at the block's end.
    for op in ops.iter().filter(|op| op.as_batched().is_none()) {
        if op.run_on(&mut access).is_err() {
            break;
        }
    }
    true
}

/// A deferred storage operation's view of the store ahead of the committer: the store at its
/// transaction's version beneath the writes of the operations run before it.
struct WarmAccess<'m, 's, B: ?Sized> {
    store: &'m MvStore<'s, B>,
    version: u32,
    slots: &'m mut SlotMap<Word>,
}

impl<B: DynDatabaseRef + Sync + ?Sized> DeferredStorageStore for WarmAccess<'_, '_, B> {
    fn sload(&mut self, address: &Address, key: Word) -> DbResult<Word> {
        if let Some(value) = self.slots.get(&(*address, key)) {
            return Ok(*value);
        }
        self.store.storage(address, &key, self.version + 1)
    }

    fn sstore(&mut self, address: &Address, key: Word, value: Word) -> DbResult<()> {
        self.slots.insert((*address, key), value);
        Ok(())
    }
}

/// Executes transactions speculatively until none are left or the committer stops.
#[expect(clippy::too_many_arguments)]
fn run_worker<T, F, H, K, Q>(
    store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
    scheduler: &Scheduler,
    queue: &TxQueue<T::Tx>,
    make_evm: &F,
    on_speculative: &H,
    copy_receipt: &K,
    results: &Results<Speculative<T, Q>>,
    worker: usize,
) where
    T: EvmTypes<Tx: Typed2718>,
    F: for<'x> Fn(Box<dyn DynDatabase + 'x>) -> Evm<'x, T>,
    H: Fn(usize, TxStateChanges<'_>),
    K: Fn(&T::Tx, &TxResult<T>) -> Option<Q>,
{
    let _exit = WorkerExit(results);
    // A panic outside the guarded execution, such as in `make_evm` or `on_speculative`, must not
    // leave transactions waiting on one this worker never finishes: stop the block instead.
    let _abort = AbortOnUnwind(scheduler);
    store.register_reader(worker);
    let view_version = Cell::new(0);
    let reads = RefCell::new(ReadSet::default());
    let mut stash = Vec::new();
    let mut evm = None;
    loop {
        // Validate a result near the committer before executing more: the committer then only
        // checks the writes logged since.
        if results.batched {
            prevalidate_next(store, queue, results);
            warm_deferred(store, results);
        }
        let Some(index) = scheduler.next() else { break };
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
        let receipt = outcome.as_ref().and_then(|outcome| copy_receipt(evm_tx, &outcome.result));
        let reads = core::mem::take(&mut *reads.borrow_mut());
        let lifecycle =
            outcome.as_ref().is_some_and(|outcome| has_lifecycle(&outcome.pending_state));
        results.put(
            index,
            Speculative {
                worker,
                outcome,
                reward,
                reads,
                writes,
                prevalidated: None,
                lifecycle,
                receipt,
            },
        );
    }

    // Every transaction was handed out, and the committer is still behind: execute again, ahead
    // of it, the results whose reads earlier executions have since overwritten, so that it can
    // commit them instead of executing them itself.
    let Some(evm) = &mut evm else { return };
    let mut idle = 0;
    let mut woken = false;
    while !scheduler.is_stopped() {
        // Read before looking for work: work that appears after the search changes it, so that
        // parking then returns at once.
        let epoch = results.idle_epoch();
        if results.batched
            && (prevalidate_next(store, queue, results) || warm_deferred(store, results))
        {
            idle = 0;
            woken = false;
            continue;
        }
        let index = results.revalidate.load(Ordering::Acquire);
        let len = queue.len.load(Ordering::Acquire);
        if index >= len {
            // Wait for the committer to execute a transaction again, which can make later results
            // stale, or to commit further, or to finish, without taking a core it may need.
            idle += 1;
            if idle < TAIL_SPINS {
                core::hint::spin_loop();
            } else if idle < TAIL_SPINS + TAIL_YIELDS {
                std::thread::yield_now();
            } else {
                results.park_idle(epoch);
                woken = true;
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
        // The committer wakes one worker for results to check again: wake another to share them.
        if core::mem::take(&mut woken) && index + 1 < len {
            results.wake_idle();
        }
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
            store.log_writes(version, writes.iter().chain(&speculative.writes).copied());
            let receipt =
                outcome.as_ref().and_then(|outcome| copy_receipt(evm_tx, &outcome.result));
            let reads = core::mem::take(&mut *reads.borrow_mut());
            let lifecycle =
                outcome.as_ref().is_some_and(|outcome| has_lifecycle(&outcome.pending_state));
            let stale = core::mem::replace(
                speculative,
                Speculative {
                    worker,
                    outcome,
                    reward,
                    reads,
                    writes,
                    prevalidated: None,
                    lifecycle,
                    receipt,
                },
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

/// Validates the next result shortly ahead of the committer against the store, once every result
/// up to it is in, recording the store's write log length beforehand so the committer can tell
/// whether a later write could have changed a value read. Returns whether it claimed a result.
fn prevalidate_next<T: EvmTypes, Q>(
    store: &MvStore<'_, impl DynDatabaseRef + Sync + ?Sized>,
    queue: &TxQueue<T::Tx>,
    results: &Results<Speculative<T, Q>>,
) -> bool {
    let len = queue.len.load(Ordering::Acquire);
    let committing = results.committing.load(Ordering::Acquire);
    let claimed = results.prevalidate.load(Ordering::Acquire);
    let index = claimed.max(committing + 1);
    if index >= len || index > committing + PREVALIDATE_AHEAD || index >= results.ready_prefix() {
        return false;
    }
    if results
        .prevalidate
        .compare_exchange(claimed, index + 1, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return true;
    }
    let mark = store.log_mark();
    results.update(index, |speculative| {
        let Some(outcome) = &speculative.outcome else { return };
        let valid = speculative.reads.validate_store(
            store,
            version(index),
            &outcome.pending_state,
            &queue.get(index).signer(),
        );
        speculative.prevalidated = Some((mark, valid));
    });
    true
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
        let resolved = pending
            .deferred()
            .map(|deferred| deferred.resolved.iter().copied().collect::<HashSet<_>>())
            .unwrap_or_default();
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

/// One speculative execution sent from a worker to the committer, with the copy of its receipt
/// the caller prepared, if any.
struct Speculative<T: EvmTypes, Q = ()> {
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
    /// A worker's validation of the result against the store in a batched commit, as it
    /// committed every earlier transaction: the length of the store's write log when it started,
    /// and whether every read held.
    prevalidated: Option<(usize, bool)>,
    /// Whether the execution created an account or wiped storage, which a batched commit records.
    lifecycle: bool,
    /// The copy of the receipt the result records, prepared on the worker.
    receipt: Option<Q>,
}

/// The values one speculative execution read.
#[derive(Debug, Default)]
struct ReadSet {
    /// Each account read, the value read and whether it was the state before the first
    /// transaction.
    accounts: Vec<(Address, Option<AccountInfo>, bool)>,
    /// Each slot read, the value read and the slot's value before the first transaction, when the
    /// read needed it.
    storage: Vec<(Address, Word, Word, Option<Word>)>,
    /// Accounts whose committed balance differs from the one read, which the execution did not
    /// read exactly: the balance read and the committed one, filled by [`Self::validate`].
    rebase: Vec<(Address, Word, Word)>,
    /// The [`key_bit`] of every account and slot read, so that most keys it did not read are
    /// ruled out without searching the reads.
    filter: u64,
    /// The [`key_bit`] of the address of every slot read.
    slot_accounts: u64,
}

impl ReadSet {
    /// Records a read of the account at `address`.
    fn read_account(&mut self, address: Address, value: Option<AccountInfo>, initial: bool) {
        self.filter |= key_bit(&address, None);
        self.accounts.push((address, value, initial));
    }

    /// Records a read of the slot `key` of `address`.
    fn read_storage(&mut self, address: Address, key: Word, value: Word, initial: Option<Word>) {
        self.filter |= key_bit(&address, Some(&key));
        self.slot_accounts |= key_bit(&address, None);
        self.storage.push((address, key, value, initial));
    }

    /// Returns whether the execution may have read the value a key with [`WriteKey::bit`] `bit`
    /// writes: `false` rules it out.
    const fn may_touch(&self, (bit, wipe): (u64, bool)) -> bool {
        (if wipe { self.slot_accounts } else { self.filter }) & bit != 0
    }

    /// Returns whether the execution read the value `key` writes.
    fn touches(&self, key: WriteKey) -> bool {
        match key {
            WriteKey::Account(address) => self.accounts.iter().any(|(read, ..)| *read == address),
            WriteKey::Storage(address, key) => {
                self.storage.iter().any(|(read, slot, ..)| *read == address && *slot == key)
            }
            WriteKey::Wipe(address) => self.storage.iter().any(|(read, ..)| *read == address),
        }
    }

    fn clear(&mut self) {
        self.accounts.clear();
        self.storage.clear();
        self.rebase.clear();
        self.filter = 0;
        self.slot_accounts = 0;
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
        let Self { accounts, storage, rebase, .. } = self;
        rebase.clear();
        for (address, value, _) in accounts.iter() {
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
        for (address, key, value, _) in storage.iter() {
            if evm.read_storage(address, key)? != *value {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl ReadSet {
    /// Returns whether every read sees the same value in `store` at `version`, where every
    /// earlier transaction committed, or only a balance the execution of `pending`, sent by
    /// `sender`, did not read exactly, which it records to rebase. The store-backed counterpart of
    /// [`Self::validate`].
    fn validate_store<B: DynDatabaseRef + Sync + ?Sized>(
        &mut self,
        store: &MvStore<'_, B>,
        version: u32,
        pending: &PendingState,
        sender: &Address,
    ) -> bool {
        let Self { accounts, storage, rebase, .. } = self;
        rebase.clear();
        for (address, value, initial) in accounts.iter() {
            // A read of the state before the first transaction still holds while no transaction
            // below `version` wrote the account.
            let Ok(now) = store.account_unless_initial(address, version, *initial) else {
                return false;
            };
            if let Some(mut now) = now {
                if let Some(info) = &mut now
                    && info.code_hash.is_zero()
                {
                    info.code_hash = KECCAK256_EMPTY;
                }
                let now = now.as_ref().map(AccountInfo::clone_no_code);
                if now != *value {
                    let Some(floor) = delta_floor(pending, sender, address) else { return false };
                    let (Some(read), Some(committed)) = (value, &now) else { return false };
                    if !balance_only_change(read, committed, floor) {
                        return false;
                    }
                    match rebase.iter().find(|(rebased, ..)| rebased == address) {
                        // Reads of one account at different versions cannot both be rebased.
                        Some(&(_, balance, _)) if balance != read.balance => return false,
                        Some(_) => {}
                        None => rebase.push((*address, read.balance, committed.balance)),
                    }
                }
            }
            // A worker reads every slot of an account it read as absent as zero, without asking
            // the store, while the state before the block may hold slots of such an account.
            if value.is_none()
                && store
                    .initial
                    .storage
                    .get(address)
                    .is_some_and(|storage| !storage.slots.is_empty())
            {
                return false;
            }
        }
        storage.iter().all(|(address, key, value, initial)| {
            store.storage_from(address, key, version, *initial).is_ok_and(|(now, _)| now == *value)
        })
    }

    /// Returns whether every read still sees the same value in `store` at `version`: the latest
    /// writes of earlier transactions, committed or speculative.
    fn validate_at<B: DynDatabaseRef + Sync + ?Sized>(
        &self,
        store: &MvStore<'_, B>,
        version: u32,
        pending: &PendingState,
        sender: &Address,
    ) -> bool {
        self.accounts.iter().all(|(address, value, initial)| {
            store.account_unless_initial(address, version, *initial).is_ok_and(|info| {
                let Some(info) = info else { return true };
                let info = info.as_ref().map(AccountInfo::clone_no_code);
                info == *value
                    || matches!((value, &info), (Some(read), Some(now))
                        if delta_floor(pending, sender, address)
                            .is_some_and(|floor| balance_only_change(read, now, floor)))
            })
        }) && self.storage.iter().all(|(address, key, value, initial)| {
            store.storage_from(address, key, version, *initial).is_ok_and(|(v, _)| v == *value)
        })
    }
}

/// Returns the lowest balance of `address` the execution of `pending`, sent by `sender`, needs
/// when it did not read the balance exactly, or `None` when it did. The sender's balance pays
/// for gas, which the execution checks exactly.
fn delta_floor(pending: &PendingState, sender: &Address, address: &Address) -> Option<Word> {
    if address == sender {
        return None;
    }
    let Some(reads) = pending.balance_reads() else { return Some(Word::ZERO) };
    if reads.observed.contains(address) {
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

/// Returns the bit of a [`ReadSet`] filter that stands for the account at `address`, or for its
/// slot `key`.
#[inline]
fn key_bit(address: &Address, key: Option<&Word>) -> u64 {
    let mut hash = u64::from_le_bytes(address[12..].try_into().expect("8 bytes"));
    if let Some(key) = key {
        hash ^= key.as_limbs()[0].rotate_left(29) ^ key.as_limbs()[3];
    }
    1 << (hash.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 58)
}

/// A key a transaction published to the multi-version store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum WriteKey {
    Account(Address),
    Storage(Address, Word),
    Wipe(Address),
}

impl WriteKey {
    /// Returns the [`ReadSet`] filter bit of the values the key writes, and whether it is a wipe,
    /// which writes every slot of its account.
    fn bit(self) -> (u64, bool) {
        match self {
            Self::Account(address) => (key_bit(&address, None), false),
            Self::Storage(address, key) => (key_bit(&address, Some(&key)), false),
            Self::Wipe(address) => (key_bit(&address, None), true),
        }
    }
}

/// The keys written again after their transaction's first execution published, in order.
#[derive(Debug, Default)]
struct WriteLog {
    /// Each key with the version written.
    keys: Vec<(u32, WriteKey)>,
    /// Each key's version and [`WriteKey::bit`], to rule most keys out without reading them.
    bits: Vec<(u32, (u64, bool))>,
}

/// One lock shard of account versions.
type AccountShard = ShardLock<AddressMap<Versions<Option<AccountInfo>>>>;

/// One lock shard of storage slot versions.
type StorageShard = ShardLock<SlotMap<Versions<SlotWrite>>>;

/// A map keyed by storage slot, with [`SlotHasher`].
type SlotMap<V> = HashMap<(Address, Word), V, SlotState>;

/// Builds the [`SlotHasher`]s of a [`SlotMap`] from a random seed drawn once per process, as the
/// default map hasher is seeded.
#[derive(Clone, Copy, Debug)]
struct SlotState(u64);

impl Default for SlotState {
    fn default() -> Self {
        static SEED: OnceLock<u64> = OnceLock::new();
        Self(*SEED.get_or_init(|| RandomState::new().hash_one(0u64)))
    }
}

impl BuildHasher for SlotState {
    type Hasher = SlotHasher;

    #[inline]
    fn build_hasher(&self) -> SlotHasher {
        SlotHasher(self.0)
    }
}

/// Hashes the fixed-size `(Address, Word)` keys of a [`SlotMap`] a word at a time.
#[derive(Clone, Copy, Debug)]
struct SlotHasher(u64);

impl Hasher for SlotHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u64(&mut self, word: u64) {
        self.0 = (self.0 ^ word).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(26);
    }

    /// Keys have a fixed size, so their length prefixes carry nothing.
    #[inline]
    fn write_usize(&mut self, _: usize) {}

    #[inline]
    fn finish(&self) -> u64 {
        self.0.wrapping_mul(0xbf58_476d_1ce4_e5b9)
    }
}

/// A transaction's write to a storage slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotWrite {
    /// The value it left.
    Value(Word),
    /// A wrapping sum of blind adds to whatever value it found.
    Delta(Word),
}

/// The base database values one thread read for one [`MvStore`].
#[derive(Debug, Default)]
struct BaseMemo {
    /// The identifier of the store the values belong to.
    store: usize,
    accounts: AddressMap<Option<AccountInfo>>,
    storage: SlotMap<Word>,
}

impl BaseMemo {
    /// Returns the memo of the store with identifier `store`, clearing the values of another.
    fn of(&mut self, store: usize) -> &mut Self {
        if self.store != store {
            self.store = store;
            self.accounts.clear();
            self.storage.clear();
        }
        self
    }
}

/// The keys that may have a version in a [`MvStore`], by hash: a key whose bit is clear has none,
/// so reading it skips the lock and the map of its shard. A key is marked before its first version
/// is written and never unmarked.
struct WriteFilter(Box<[AtomicU64]>);

impl WriteFilter {
    fn new() -> Self {
        Self((0..1usize << (FILTER_BITS - 6)).map(|_| AtomicU64::new(0)).collect())
    }

    /// Returns the word holding the bit of `hash`, and the bit.
    #[inline]
    fn bit(&self, hash: u64) -> (&AtomicU64, u64) {
        let index = (hash >> (64 - FILTER_BITS)) as usize;
        (&self.0[index >> 6], 1 << (index & 63))
    }

    /// Marks `hash`, before a version of its key is written.
    #[inline]
    fn insert(&self, hash: u64) {
        let (word, bit) = self.bit(hash);
        // A marked bit is left alone, so that its cache line stays shared between readers.
        if word.load(Ordering::Relaxed) & bit == 0 {
            word.fetch_or(bit, Ordering::Release);
        }
    }

    /// Returns whether the key of `hash` may have a version. A version written before the read,
    /// such as by a transaction whose result the reader took, always sets the bit first.
    #[inline]
    fn contains(&self, hash: u64) -> bool {
        let (word, bit) = self.bit(hash);
        word.load(Ordering::Acquire) & bit != 0
    }
}

/// Returns the [`WriteFilter`] hash of the account at `address`.
#[inline]
fn account_hash(address: &Address) -> u64 {
    let low = u64::from_le_bytes(address[12..].try_into().expect("8 bytes"));
    let high = u64::from_le_bytes(address[..8].try_into().expect("8 bytes"));
    (low ^ high.rotate_left(32)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Returns the [`WriteFilter`] hash of the slot `key` of `address`.
#[inline]
fn slot_hash(address: &Address, key: &Word) -> u64 {
    let [a, b, c, d] = *key.as_limbs();
    let key = (a ^ b.rotate_left(16) ^ c.rotate_left(32) ^ d.rotate_left(48))
        .wrapping_mul(0xbf58_476d_1ce4_e5b9);
    account_hash(address) ^ key
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
    /// Identifies the store's reads of `base` in [`BASE_MEMO`].
    id: usize,
    accounts: Box<[CachePadded<AccountShard>]>,
    storage: Box<[CachePadded<StorageShard>]>,
    /// The reader slots of the account and storage shards: one per worker, then the committer's.
    readers: Readers,
    /// The accounts that may have a version in `accounts`.
    written_accounts: WriteFilter,
    /// The slots that may have a version in `storage`.
    written_slots: WriteFilter,
    wipes: RwLock<AddressMap<BTreeSet<u32>>>,
    has_wipes: AtomicBool,
    contracts: RwLock<B256Map<Bytecode>>,
    /// Keys written again after their transaction's first execution published, with the version
    /// written: what can change a value a validation read.
    log: Mutex<WriteLog>,
    log_len: AtomicUsize,
}

impl<'s, B: DynDatabaseRef + Sync + ?Sized> MvStore<'s, B> {
    /// Returns a store with reader slots for `workers` workers and the committer.
    fn new(initial: Cache, base: &'s B, workers: usize) -> Self {
        let id = NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            initial,
            base,
            id,
            accounts: (0..SHARDS)
                .map(|shard| CachePadded(ShardLock::new(1 + shard, Default::default())))
                .collect(),
            storage: (0..SHARDS)
                .map(|shard| CachePadded(ShardLock::new(1 + SHARDS + shard, Default::default())))
                .collect(),
            readers: Readers::new(id, workers + 1),
            written_accounts: WriteFilter::new(),
            written_slots: WriteFilter::new(),
            wipes: RwLock::default(),
            has_wipes: AtomicBool::new(false),
            contracts: RwLock::default(),
            log: Mutex::default(),
            log_len: AtomicUsize::new(0),
        }
    }

    /// Registers the calling thread as the reader in `slot`: a worker's index, or the number of
    /// workers for the committer.
    fn register_reader(&self, slot: usize) {
        self.readers.register(slot);
    }

    /// Appends keys written at `version` to the write log, after writing them.
    fn log_writes(&self, version: u32, keys: impl IntoIterator<Item = WriteKey>) {
        let mut log = self.log.lock().expect("lock poisoned");
        for key in keys {
            log.keys.push((version, key));
            log.bits.push((version, key.bit()));
        }
        self.log_len.store(log.keys.len(), Ordering::Release);
    }

    /// Returns the write log's length, to take before reading values to validate.
    fn log_mark(&self) -> usize {
        self.log_len.load(Ordering::Acquire)
    }

    /// Returns whether a key logged since `mark` below `version` is one `reads` read.
    fn logged_since(&self, mark: usize, version: u32, reads: &ReadSet) -> bool {
        // An append still in flight orders after this check, as if it took the lock later.
        if self.log_mark() == mark {
            return false;
        }
        let log = self.log.lock().expect("lock poisoned");
        log.bits[mark..].iter().zip(&log.keys[mark..]).any(|(&(at, bit), &(_, key))| {
            at < version && reads.may_touch(bit) && reads.touches(key)
        })
    }

    #[inline]
    fn account_shard(&self, address: &Address) -> &AccountShard {
        &self.accounts[address[19] as usize % SHARDS]
    }

    #[inline]
    fn storage_shard(&self, address: &Address, key: &Word) -> &StorageShard {
        &self.storage[storage_shard_index(address, key)]
    }

    /// Locks the shard of the account at `address` to write a version of it, marking it written.
    fn write_account_shard(
        &self,
        address: &Address,
    ) -> WriteGuard<'_, AddressMap<Versions<Option<AccountInfo>>>> {
        self.written_accounts.insert(account_hash(address));
        self.account_shard(address).write(&self.readers)
    }

    /// Locks the shard of the slot `key` of `address` to write a version of it, marking it
    /// written.
    fn write_storage_shard(
        &self,
        address: &Address,
        key: &Word,
    ) -> WriteGuard<'_, SlotMap<Versions<SlotWrite>>> {
        self.written_slots.insert(slot_hash(address, key));
        self.storage_shard(address, key).write(&self.readers)
    }

    /// Returns the latest account write below `version`.
    fn account_version(&self, address: &Address, version: u32) -> Option<Option<AccountInfo>> {
        if !self.written_accounts.contains(account_hash(address)) {
            return None;
        }
        let shard = self.account_shard(address).read(&self.readers);
        let (_, info) = shard.get(address)?.latest_below(version)?;
        Some(info.clone())
    }

    /// Returns the account before the first transaction.
    fn account_initial(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        match self.initial.accounts.get(address) {
            Some(info) => Ok(info.clone()),
            None => self.base_account(address),
        }
    }

    /// Reads an account from the base database, once per thread.
    fn base_account(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        BASE_MEMO.with_borrow_mut(|memo| {
            let memo = memo.of(self.id);
            if let Some(info) = memo.accounts.get(address) {
                return Ok(info.clone());
            }
            let info = self.base.get_account(address)?;
            memo.accounts.insert(*address, info.clone());
            Ok(info)
        })
    }

    /// Reads a storage slot from the base database, once per thread.
    fn base_storage(&self, address: &Address, key: &Word) -> DbResult<Word> {
        BASE_MEMO.with_borrow_mut(|memo| {
            let memo = memo.of(self.id);
            if let Some(value) = memo.storage.get(&(*address, *key)) {
                return Ok(*value);
            }
            let value = self.base.get_storage(address, key)?;
            memo.storage.insert((*address, *key), value);
            Ok(value)
        })
    }

    /// Reads an account at `version`.
    fn account(&self, address: &Address, version: u32) -> DbResult<Option<AccountInfo>> {
        self.account_read(address, version).map(|(info, _)| info)
    }

    /// Reads an account at `version`, and whether it read the state before the first
    /// transaction.
    fn account_read(
        &self,
        address: &Address,
        version: u32,
    ) -> DbResult<(Option<AccountInfo>, bool)> {
        match self.account_version(address, version) {
            Some(info) => Ok((info, false)),
            None => self.account_initial(address).map(|info| (info, true)),
        }
    }

    /// Reads an account at `version` to validate a read of it, or returns `None` when no
    /// transaction below `version` wrote it and the read, `initial`, returned the state before
    /// the first transaction, which no transaction changes.
    fn account_unless_initial(
        &self,
        address: &Address,
        version: u32,
        initial: bool,
    ) -> DbResult<Option<Option<AccountInfo>>> {
        match self.account_version(address, version) {
            Some(info) => Ok(Some(info)),
            None if initial => Ok(None),
            None => self.account_initial(address).map(Some),
        }
    }

    /// Reads a storage slot at `version`: the latest value written below it, or zero after a later
    /// wipe, then the state before the first transaction, plus every delta published above that.
    fn storage(&self, address: &Address, key: &Word, version: u32) -> DbResult<Word> {
        self.storage_from(address, key, version, None).map(|(value, _)| value)
    }

    /// Reads a storage slot at `version` as [`Self::storage`] does, taking the slot's value
    /// before the first transaction from `initial` when given, and returns the value with the one
    /// before the first transaction, when the read needed it.
    fn storage_from(
        &self,
        address: &Address,
        key: &Word,
        version: u32,
        initial: Option<Word>,
    ) -> DbResult<(Word, Option<Word>)> {
        let wipe = if self.has_wipes.load(Ordering::Acquire) {
            let wipes = self.wipes.read().expect("lock poisoned");
            wipes.get(address).and_then(|wipes| wipes.range(..version).next_back().copied())
        } else {
            None
        };
        let (mut value, mut delta) = (None, Word::ZERO);
        if self.written_slots.contains(slot_hash(address, key)) {
            let shard = self.storage_shard(address, key).read(&self.readers);
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
        let (value, initial) = match (value, wipe) {
            (Some(value), _) => (value, None),
            (None, Some(_)) => (Word::ZERO, None),
            (None, None) => {
                let initial = match initial {
                    Some(initial) => initial,
                    None => self.storage_initial(address, key)?,
                };
                (initial, Some(initial))
            }
        };
        Ok((value.wrapping_add(delta), initial))
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
        self.base_storage(address, key)
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
                let mut shard = self.write_storage_shard(&address, &key);
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
                let mut shard = self.write_account_shard(&address);
                shard.entry(address).or_default().insert(version, info);
                writes.push(WriteKey::Account(address));
            }
        }
        // Unresolved blind adds publish as deltas, unless the transaction loaded the slot too.
        for (key, pending_delta) in
            pending.deferred().into_iter().flat_map(|deferred| &deferred.deltas)
        {
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
            let mut shard = self.write_storage_shard(&address, &key);
            shard.entry((address, key)).or_default().insert(version, write);
            writes.push(WriteKey::Storage(address, key));
        }
    }

    /// Removes the writes a speculative execution published at `version`.
    fn unpublish<'w>(&self, version: u32, writes: impl IntoIterator<Item = &'w WriteKey>) {
        for write in writes {
            match write {
                WriteKey::Account(address) => {
                    let mut shard = self.account_shard(address).write(&self.readers);
                    if let Some(versions) = shard.get_mut(address) {
                        versions.remove(version);
                    }
                }
                WriteKey::Storage(address, key) => {
                    let mut shard = self.storage_shard(address, key).write(&self.readers);
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
        {
            let mut shard = self.write_account_shard(&address);
            shard.entry(address).or_default().insert(version, info.map(AccountInfo::clone_no_code));
        }
        self.log_writes(version, [WriteKey::Account(address)]);
    }

    /// Writes a slot's value at `version` without logging it.
    fn put_slot(&self, version: u32, address: Address, key: Word, value: Word) {
        let mut shard = self.write_storage_shard(&address, &key);
        shard.entry((address, key)).or_default().insert(version, SlotWrite::Value(value));
    }

    /// Writes slots' values at `version` as [`Self::put_slot`] does, locking each shard once.
    /// Sorts `writes` by shard, keeping the order of writes to one slot.
    fn put_slots(&self, version: u32, writes: &mut [(Address, Word, Word)]) {
        writes.sort_by_key(|(address, key, _)| storage_shard_index(address, key));
        for shard_writes in writes
            .chunk_by(|(a, x, _), (b, y, _)| storage_shard_index(a, x) == storage_shard_index(b, y))
        {
            let (address, key, _) = &shard_writes[0];
            let mut shard = self.storage_shard(address, key).write(&self.readers);
            shard.reserve(shard_writes.len());
            for &(address, key, value) in shard_writes {
                self.written_slots.insert(slot_hash(&address, &key));
                shard.entry((address, key)).or_default().insert(version, SlotWrite::Value(value));
            }
        }
    }

    /// Returns the block's changes once every transaction committed: each account and slot written
    /// from its state before the block to its latest value. Threads read the shards' values in
    /// parallel.
    fn materialize(&self) -> DbResult<PendingState> {
        let threads = std::thread::available_parallelism()
            .map_or(1, NonZeroUsize::get)
            .clamp(1, MATERIALIZE_THREADS);
        let chunk = SHARDS.div_ceil(threads);
        let parts = std::thread::scope(|scope| {
            let spawned = (1..threads)
                .map(|part| scope.spawn(move || self.materialize_shards(part * chunk, chunk)))
                .collect::<Vec<_>>();
            let mut parts = vec![self.materialize_shards(0, chunk)];
            for handle in spawned {
                parts.push(handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)));
            }
            parts
        });
        let mut state = PendingState::default();
        let mut slots = Vec::new();
        for part in parts {
            let (accounts, part_slots) = part?;
            for (address, original, present) in accounts {
                state.insert_account(address, original, present);
            }
            slots.push(part_slots);
        }
        for (address, key, original, current) in slots.into_iter().flatten() {
            if !state.accounts.contains_key(&address) {
                let info = self.account_initial(&address)?;
                state.insert_account(address, info.clone(), info);
            }
            state.insert_storage(address, key, original, current);
        }
        Ok(state)
    }

    /// Marks in the block's changes `state` the accounts its transactions created or whose
    /// storage they wiped.
    fn apply_lifecycle(
        &self,
        state: &mut PendingState,
        lifecycle: &AddressMap<Lifecycle>,
    ) -> DbResult<()> {
        for (address, lifecycle) in lifecycle {
            if !state.accounts.contains_key(address) {
                let info = self.account_initial(address)?;
                state.insert_account(*address, info.clone(), info);
            }
            let account = state.accounts.get_mut(address).expect("inserted above");
            account.storage.wiped |= lifecycle.wiped;
            if lifecycle.created {
                account.mark_created_discarding_storage();
            }
        }
        Ok(())
    }

    /// Reads the accounts and slots written in `len` account and storage shards from `start`: each
    /// account before the block and at its latest value, and each slot before the block and at its
    /// latest value.
    #[expect(clippy::type_complexity)]
    fn materialize_shards(
        &self,
        start: usize,
        len: usize,
    ) -> DbResult<(
        Vec<(Address, Option<AccountInfo>, Option<AccountInfo>)>,
        Vec<(Address, Word, Word, Word)>,
    )> {
        let range = start.min(SHARDS)..(start + len).min(SHARDS);
        let mut accounts = Vec::new();
        for shard in &self.accounts[range.clone()] {
            for (address, versions) in shard.read(&self.readers).iter() {
                let Some((_, present)) = versions.latest_below(u32::MAX) else { continue };
                let original = self.account_initial(address)?;
                let mut present = present.clone();
                if let Some(info) = &mut present
                    && original.as_ref().is_none_or(|original| original.code_hash != info.code_hash)
                    && info.code_hash != KECCAK256_EMPTY
                    && !info.code_hash.is_zero()
                {
                    info.code = Some(self.code(&info.code_hash)?);
                }
                accounts.push((*address, original, present));
            }
        }
        let mut slots = Vec::new();
        for shard in &self.storage[range] {
            for ((address, key), versions) in shard.read(&self.readers).iter() {
                if versions.latest_below(u32::MAX).is_none() {
                    continue;
                }
                let current = self.storage(address, key, u32::MAX)?;
                let original = self.storage_initial(address, key)?;
                slots.push((*address, *key, original, current));
            }
        }
        Ok((accounts, slots))
    }

    /// Frees the store's versions on a background thread.
    fn drop_in_background(self) {
        let Self { initial, accounts, storage, wipes, contracts, log, .. } = self;
        drop_in_background((initial, accounts, storage, wipes, contracts, log));
    }

    /// Publishes at `version` the slots that resolving a committed transaction's deferred storage
    /// changed: the values of blind adds it never read and the slots its deferred operations wrote,
    /// which its speculative execution could not publish.
    fn publish_resolved(&self, version: u32, pending: &PendingState) {
        for &(address, key) in
            pending.deferred().into_iter().flat_map(|deferred| &deferred.resolved)
        {
            if let Some(account) = pending.accounts.get(&address)
                && let Some(slot) = account.storage.slots.get(&key)
                && (slot.value.is_changed() || account.storage.wiped)
            {
                let mut shard = self.write_storage_shard(&address, &key);
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
        let (mut info, initial) = self.store.account_read(address, self.version.get())?;
        // Record the account as the executor's database returns it, which maps a zero code hash
        // to the empty one.
        if let Some(info) = &mut info
            && info.code_hash.is_zero()
        {
            info.code_hash = KECCAK256_EMPTY;
        }
        let read = info.as_ref().map(AccountInfo::clone_no_code);
        self.reads.borrow_mut().read_account(*address, read, initial);
        Ok(info)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        // Code is content-addressed: a hash reads the same bytes at every version.
        self.store.code(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let (value, initial) = self.store.storage_from(address, key, self.version.get(), None)?;
        self.reads.borrow_mut().read_storage(*address, *key, value, initial);
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
    /// Index of the next result for an idle worker to validate ahead of a batched commit.
    prevalidate: CachePadded<AtomicUsize>,
    /// Number of leading results stored.
    ready: CachePadded<AtomicUsize>,
    /// Whether the committer commits in batch, so idle workers validate ahead of it.
    batched: bool,
    /// The deferred storage operations idle workers run ahead of a batched commit.
    warmer: Mutex<DeferredWarmer>,
    /// Advanced whenever work for idle workers may have appeared.
    idle_epoch: CachePadded<AtomicUsize>,
    /// Number of parked idle workers.
    sleepers: CachePadded<AtomicUsize>,
    /// The parked idle workers, for a wake to unpark.
    sleeping: Mutex<Vec<Thread>>,
}

impl<S> Results<S> {
    /// Results produced by `workers` threads for the calling thread to take.
    fn new(workers: usize, batched: bool) -> Self {
        Self {
            slots: Buckets::default(),
            committer: std::thread::current(),
            waiting: AtomicUsize::new(usize::MAX),
            workers: AtomicUsize::new(workers),
            pools: (0..workers).map(|_| CachePadded::default()).collect(),
            committing: CachePadded(AtomicUsize::new(0)),
            revalidate: CachePadded(AtomicUsize::new(0)),
            prevalidate: CachePadded(AtomicUsize::new(0)),
            ready: CachePadded(AtomicUsize::new(0)),
            batched,
            warmer: Mutex::default(),
            idle_epoch: CachePadded(AtomicUsize::new(0)),
            sleepers: CachePadded(AtomicUsize::new(0)),
            sleeping: Mutex::default(),
        }
    }

    /// Stores the result of the transaction at `index`, waking the committer if it waits for it.
    fn put(&self, index: usize, result: S) {
        let slot = self.slots.get(index);
        // SAFETY: the slot is empty, so no other thread accesses its value until it is ready.
        unsafe { *slot.value.get() = Some(result) };
        slot.state.store(READY, Ordering::Release);
        // Advance the stored prefix past every leading result now in.
        let mut ready = self.ready.load(Ordering::Acquire);
        while self.slots.get(ready).is_ready() {
            match self.ready.compare_exchange_weak(
                ready,
                ready + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => ready += 1,
                Err(current) => ready = current,
            }
        }
        // Pairs with the fence in `take`: either it sees the result or this sees it waiting.
        fence(Ordering::SeqCst);
        if self.waiting.load(Ordering::Relaxed) == index {
            self.committer.unpark();
        }
        // A result next to the committer can be validated ahead of it.
        self.wake_idle();
    }

    /// Takes the result of the transaction at `index`, waiting for it, or returns `None` once
    /// every worker exited without producing it.
    fn take(&self, index: usize) -> Option<S> {
        self.committing.store(index, Ordering::Relaxed);
        // Committing further lets idle workers validate further ahead: wake one now and then,
        // since it validates every result in reach.
        if index.is_multiple_of(IDLE_WAKE_COMMITS) {
            self.wake_idle();
        }
        let slot = self.slots.get(index);
        let mut spins = 0;
        while !slot.is_ready() {
            if spins < COMMIT_SPINS {
                spins += 1;
                core::hint::spin_loop();
                continue;
            }
            self.waiting.store(index, Ordering::Relaxed);
            fence(Ordering::SeqCst);
            if slot.is_ready() {
                break;
            }
            if self.workers.load(Ordering::Acquire) == 0 {
                // A worker stores its last result before it exits.
                if !slot.is_ready() {
                    self.waiting.store(usize::MAX, Ordering::Relaxed);
                    return None;
                }
                break;
            }
            std::thread::park();
        }
        self.waiting.store(usize::MAX, Ordering::Relaxed);
        let mut spins = 0;
        while slot
            .state
            .compare_exchange_weak(READY, HELD, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // A worker validates or executes the result again.
            if spins < COMMIT_SPINS {
                spins += 1;
                core::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        // SAFETY: holding the slot excludes every other access to its value.
        let result = unsafe { (*slot.value.get()).take() };
        slot.state.store(READY, Ordering::Release);
        result
    }

    /// Returns the idle epoch, which [`Self::park_idle`] waits to change.
    fn idle_epoch(&self) -> usize {
        self.idle_epoch.load(Ordering::SeqCst)
    }

    /// Parks the calling idle worker until a wake after the idle epoch was `epoch`, or at most
    /// [`TAIL_PARK`]. Returns at once when the epoch changed already.
    fn park_idle(&self, epoch: usize) {
        let current = std::thread::current();
        {
            let mut sleeping = self.sleeping.lock().expect("lock poisoned");
            // Count in before reading the epoch: a wake that advances it after the read then sees
            // this worker counted and unparks it.
            self.sleepers.fetch_add(1, Ordering::SeqCst);
            if self.idle_epoch.load(Ordering::SeqCst) != epoch {
                self.sleepers.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            sleeping.push(current.clone());
        }
        std::thread::park_timeout(TAIL_PARK);
        let mut sleeping = self.sleeping.lock().expect("lock poisoned");
        if let Some(position) = sleeping.iter().position(|thread| thread.id() == current.id()) {
            sleeping.swap_remove(position);
            self.sleepers.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Advances the idle epoch and unparks one parked idle worker, if any.
    fn wake_idle(&self) {
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);
        if self.sleepers.load(Ordering::SeqCst) == 0 {
            return;
        }
        let mut sleeping = self.sleeping.lock().expect("lock poisoned");
        let Some(thread) = sleeping.pop() else { return };
        self.sleepers.fetch_sub(1, Ordering::Relaxed);
        drop(sleeping);
        thread.unpark();
    }

    /// Advances the idle epoch and unparks every parked idle worker.
    fn wake_all_idle(&self) {
        self.idle_epoch.fetch_add(1, Ordering::SeqCst);
        let threads = {
            let mut sleeping = self.sleeping.lock().expect("lock poisoned");
            self.sleepers.store(0, Ordering::Relaxed);
            core::mem::take(&mut *sleeping)
        };
        for thread in threads {
            thread.unpark();
        }
    }

    /// Returns the number of leading results stored.
    fn ready_prefix(&self) -> usize {
        self.ready.load(Ordering::Acquire)
    }

    /// Runs `f` on the result at `index` while it holds the result's lock, if the result is
    /// stored, the committer has not taken it and no other worker holds it. The committer waits
    /// for `f` to finish before it takes the result. Returns `false` when the result is not
    /// stored yet or another thread holds it.
    fn update(&self, index: usize, f: impl FnOnce(&mut S)) -> bool {
        let slot = self.slots.get(index);
        if slot.state.compare_exchange(READY, HELD, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return false;
        }
        // Releases the slot even when `f` unwinds.
        let _release = SlotRelease(&slot.state);
        // SAFETY: holding the slot excludes every other access to its value.
        if let Some(result) = unsafe { (*slot.value.get()).as_mut() } {
            f(result);
        }
        true
    }

    /// Returns buffers for `worker`, with empty reads and writes, from its `stash`, which it
    /// refills with every buffer handed back since it last ran out.
    fn buffers(&self, worker: usize, stash: &mut Vec<Buffers>) -> Buffers {
        if stash.is_empty() {
            core::mem::swap(stash, &mut *self.pools[worker].lock().expect("lock poisoned"));
        }
        let Some(mut buffers) = stash.pop() else { return Buffers::default() };
        buffers.reads.clear();
        buffers.writes.clear();
        if let Some(parallel) = &mut buffers.pending.parallel {
            parallel.deferred.clear();
        }
        buffers
    }

    /// Frees the buffers handed back on a background thread.
    fn drop_buffers_in_background(&self) {
        let buffers = self
            .pools
            .iter()
            .map(|pool| core::mem::take(&mut *pool.lock().expect("lock poisoned")))
            .collect::<Vec<_>>();
        drop_in_background(buffers);
    }

    /// Hands buffers back to the `worker` that filled them.
    fn recycle(&self, worker: usize, buffers: Buffers) {
        self.pools[worker].lock().expect("lock poisoned").push(buffers);
    }

    /// Hands every buffer of `buffers` back to the `worker` that filled them, leaving it empty.
    fn recycle_all(&self, worker: usize, buffers: &mut Vec<Buffers>) {
        if !buffers.is_empty() {
            self.pools[worker].lock().expect("lock poisoned").append(buffers);
        }
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
    /// [`EMPTY`] until `value` is stored, then [`READY`], or [`HELD`] while one thread
    /// accesses `value`.
    state: AtomicU8,
    value: UnsafeCell<Option<S>>,
}

impl<S> ResultSlot<S> {
    /// Returns whether the result was stored.
    #[inline]
    fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) != EMPTY
    }
}

impl<S> Default for ResultSlot<S> {
    fn default() -> Self {
        Self { state: AtomicU8::new(EMPTY), value: UnsafeCell::new(None) }
    }
}

// SAFETY: `state` hands `value` to one thread at a time.
unsafe impl<S: Send> Sync for ResultSlot<S> {}

/// Releases a held [`ResultSlot`] when dropped.
struct SlotRelease<'s>(&'s AtomicU8);

impl Drop for SlotRelease<'_> {
    fn drop(&mut self) {
        self.0.store(READY, Ordering::Release);
    }
}

/// A [`ResultSlot`] with no result stored yet.
const EMPTY: u8 = 0;

/// A [`ResultSlot`] whose result is stored and free to access.
const READY: u8 = 1;

/// A [`ResultSlot`] whose result one thread accesses.
const HELD: u8 = 2;

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

mod shard_lock;

#[cfg(test)]
mod tests;
