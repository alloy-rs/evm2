//! OSS-1032 probe: the evm2 `State` overlay of each fork is the single accepted-state authority.
//!
//! Foundry's backend shrinks to a [`Registry`]: backing database handles, saved state of inactive
//! forks, the snapshot registry, and persistent accounts. The [`Executor`] keeps the active fork's
//! state between calls, builds an [`Evm`] per call, and moves the state in and out, except that
//! [`Executor::call`] shares it, where master's `&self` calls borrow it. Cheatcodes are dispatched
//! from an [`Inspector::call`] hook, which reaches the running [`Evm`] mid-transaction.
//!
//! Only public evm2 API is used. Run the measurement with
//! `cargo test --release -p evm2 --test foundry_registry_probe -- --ignored --nocapture`.

use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{
    Address, B256, Bytes, TxKind, U256,
    map::{AddressMap, AddressSet, B256Map, HashMap},
};
use evm2::{
    BaseEvmTypes, DatabaseError, Evm, EvmFeatures, ExecutionConfig, Inspector, Precompiles, SpecId,
    Version,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::{TxEnvelope, ethereum_tx_registry, intrinsic_gas},
    evm::{
        AccountInfo, Cache, DbResult, DynDatabase, EmptyDB, PendingState, State, StateCheckpoint,
        StateSnapshot, SystemTx, TxResult,
    },
    interpreter::{GasTracker, InstrStop, Interpreter, Message, MessageKind, MessageResult, Word, op},
};
use std::{
    cell::RefCell,
    hint::black_box,
    mem,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const SPEC: SpecId = SpecId::CANCUN;
const CALLER: Address = Address::with_last_byte(0xca);
/// The test contract; its code is a script of calls, like a test function body.
const TEST: Address = Address::with_last_byte(0x7e);
const COUNTER: Address = Address::with_last_byte(0xc0);
/// A counter whose slot is never preloaded, so reading it hits the backing database.
const COLD_COUNTER: Address = Address::with_last_byte(0xc1);
/// A counter that no backing database has, deployed on one fork like a `setUp` deployment.
const DEPLOYED: Address = Address::with_last_byte(0xd0);
/// Stand-in for the cheatcode address.
const CHEATS: Address = Address::with_last_byte(0xcc);
/// Script contracts the test calls, so that with isolation their cheatcodes run inside a child.
const HANDLER: Address = Address::with_last_byte(0x4a);
const NESTED: Address = Address::with_last_byte(0x4b);
const FAILING: Address = Address::with_last_byte(0x4c);
/// Emits its calldata as a log, like an event or a `console.log` in a test.
const LOGGER: Address = Address::with_last_byte(0x10);
/// Creates a contract whose initcode writes its slot 0 and self-destructs.
const FACTORY: Address = Address::with_last_byte(0xfa);

/// `selectFork(arg)`.
const SELECT_FORK: u8 = 1;
/// `snapshotState()`; ids are assigned in order from 0.
const SNAPSHOT: u8 = 2;
/// `revertToState(arg)`.
const REVERT_TO: u8 = 3;
/// `vm.transact`-like nested transaction against `COUNTER` (arg 0) or `COLD_COUNTER` (arg 1),
/// published to the accepted overlay mid-transaction.
const TRANSACT: u8 = 4;
/// Writes `COUNTER` slot 0 = 100 like `loadAllocs` and `cloneAccount`: through the transaction
/// layer, as master writes them to the journal.
const STATE_WRITE: u8 = 5;
/// `makePersistent(arg)`, where `arg` is the address's last byte. Changes only the registry.
const MAKE_PERSISTENT: u8 = 6;

/// Read-only backing database standing in for `SharedBackend`: shared by clones, `Send + Sync`,
/// and never written by commits.
#[derive(Debug, Default)]
struct Backing {
    accounts: AddressMap<AccountInfo>,
    code: B256Map<Bytecode>,
    storage: HashMap<(Address, Word), Word>,
    reads: AtomicUsize,
    fail_storage: AtomicBool,
}

#[derive(Clone, Debug)]
struct BackingDb(Arc<Backing>);

impl BackingDb {
    fn with_counter(value: u64) -> Self {
        let mut backing = Backing::default();
        for address in [COUNTER, COLD_COUNTER] {
            let info = AccountInfo::default().with_code(counter_code());
            backing.code.insert(info.code_hash, counter_code());
            backing.accounts.insert(address, info);
            backing.storage.insert((address, Word::ZERO), Word::from(value));
        }
        Self(Arc::new(backing))
    }

    fn reads(&self) -> usize {
        self.0.reads.load(Ordering::Relaxed)
    }

    fn fail_storage(&self, fail: bool) {
        self.0.fail_storage.store(fail, Ordering::Relaxed);
    }
}

impl DynDatabase for BackingDb {
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.0.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.0.accounts.get(address).cloned())
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        Ok(self.0.code.get(code_hash).cloned().unwrap_or_default())
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.0.reads.fetch_add(1, Ordering::Relaxed);
        if self.0.fail_storage.load(Ordering::Relaxed) {
            return Err(DatabaseError::new(std::io::Error::other("backing read failed"), false));
        }
        Ok(self.0.storage.get(&(*address, *key)).copied().unwrap_or_default())
    }

    fn get_block_hash(&mut self, _number: &Word) -> DbResult<B256> {
        Ok(B256::ZERO)
    }
}

/// A fork's state while no [`Evm`] runs it. Unlike [`State`], it is `Send`.
#[derive(Clone, Debug)]
struct SavedState {
    /// Accepted overlay cache, moved out of the live state. Shared with a running
    /// [`Executor::call`].
    cache: Arc<Cache>,
    /// Everything else: transaction layer, journal, logs, BAL context. Captured with an empty
    /// cache, so saving never clones the cache.
    rest: StateSnapshot,
}

impl Default for SavedState {
    fn default() -> Self {
        Self::save(State::new(EmptyDB::default()))
    }
}

impl SavedState {
    /// Moves the overlay cache out of `state` and captures the rest.
    fn save(mut state: State<'_>) -> Self {
        let cache = mem::take(&mut state.overlay_db_mut().cache);
        Self { cache: Arc::new(cache), rest: state.snapshot() }
    }

    /// Rebuilds a live state over `db`, moving the cache back in.
    fn load<'a>(self, db: impl DynDatabase + 'a) -> State<'a> {
        let mut state = self.rest.into_state(db);
        state.overlay_db_mut().cache = Arc::unwrap_or_clone(self.cache);
        state
    }

    /// Finalizes and accepts the transaction layer into the overlay at the end of a committed
    /// transaction, then clears per-transaction substate.
    ///
    /// The writes can't stay pending in the transaction layer: the next transaction needs each
    /// original reset to its start value, and a commit only accepts entries that differ from their
    /// original, so a write that isn't repeated after the fork is reselected would be lost.
    ///
    /// Committing alone would skip finalization, which turns self-destructs and touches into
    /// account deletions and storage wipes. `Evm::transact` finalizes only the active fork, so a
    /// system call runs evm2's own finalization here. It doesn't bump a nonce or charge a fee, and
    /// its zero-value call touches only [`CHEATS`], which exists with code on every fork, so it
    /// changes nothing else. Finalization waits for the commit because the transaction may select
    /// the fork again and use the accounts until it ends.
    fn accept_transaction(self) -> Self {
        let mut evm = new_evm(EmptyDB::default());
        *evm.state_mut() = self.load(EmptyDB::default());
        assert!(evm.system_call(SystemTx::new(CHEATS, Bytes::new())).unwrap().commit().status);
        Self::save(mem::replace(evm.state_mut(), State::new(EmptyDB::default())))
    }

    /// Reads a slot through the transaction layer, the overlay, then `db`.
    fn storage(&self, db: BackingDb, address: Address, key: Word) -> Word {
        self.clone().load(db).storage_slot_untracked(&address, &key).unwrap()
    }

    /// Captures what [`Self::save`] keeps besides the cache, leaving `state` live.
    fn rest_of(state: &mut State<'_>) -> StateSnapshot {
        let cache = mem::take(&mut state.overlay_db_mut().cache);
        let rest = state.snapshot();
        state.overlay_db_mut().cache = cache;
        rest
    }

    /// Reads an account through the transaction layer, the overlay, then `db`.
    fn account(&self, db: BackingDb, address: Address) -> Option<AccountInfo> {
        self.clone().load(db).account_info_untracked(&address).unwrap()
    }
}

#[derive(Clone, Debug)]
struct ForkEntry {
    backing: BackingDb,
    /// `None` while the fork is active: its state then lives in the executor or the running `Evm`.
    saved: Option<SavedState>,
}

/// Like Foundry's, a snapshot covers only the fork it was taken on.
#[derive(Clone, Debug)]
struct RegistrySnapshot {
    active: usize,
    active_state: StateSnapshot,
    /// [`Cheats::selected`] when the snapshot was taken.
    selected: StateSnapshot,
}

/// What remains of Foundry's backend: no cache layer of its own.
#[derive(Clone, Debug, Default)]
struct Registry {
    forks: Vec<ForkEntry>,
    active: usize,
    /// Shared by copies of the registry, so a copy doesn't copy the snapshots' states.
    snapshots: Vec<Arc<RegistrySnapshot>>,
    persistent: AddressSet,
}

impl Registry {
    fn backing(&self, fork: usize) -> BackingDb {
        self.forks[fork].backing.clone()
    }
}

/// The cheatcode inspector. Owns the registry for the duration of one call, or shares it with the
/// executor until an [`Executor::call`] first mutates it.
struct Cheats {
    /// Mutations go through [`Self::registry_mut`].
    registry: Arc<Registry>,
    /// The active fork's accepted overlay while an [`Executor::call`] shares it. The [`Evm`]
    /// reads it through [`AcceptedView`], so the live overlay cache holds only this call's reads.
    shared_overlay: Option<Arc<Cache>>,
    /// The active fork's transaction layer, journal, and logs as it was selected, or as the call
    /// started. Foundry stores this with the fork as its `journaled_state`. A snapshot restore
    /// that leaves the fork puts it back, so only the fork's writes since are dropped.
    selected: StateSnapshot,
    /// Run depth-1 calls as isolated transactions.
    isolate: bool,
    /// Set while the inspector runs in an isolated child, whose calls aren't isolated again.
    in_child: bool,
    /// While an isolated child runs: the fork it started on, and the parent's state there without
    /// the moved overlay. Cleared while a snapshot restore in the child is in effect, since the
    /// state then comes from the snapshot instead.
    ///
    /// `prepare_isolated_state` reset the child's originals to its start values. A commit only
    /// accepts entries that differ from their original, so state the child captures on that fork
    /// gets the parent's originals back. Otherwise the parent's writes before the child would be
    /// lost once a snapshot taken in the child is restored or the fork the child left is accepted.
    child_base: Option<(usize, State<'static>)>,
    /// Snapshot restores in the running child that are still in effect, like master's
    /// `isolated_snapshot_restores`.
    child_restores: Vec<ChildRestore>,
    /// Calls running in the child, like master's `isolated_frame_checkpoints`.
    child_calls: Vec<ChildCall>,
    /// The state each fork the root frame touched had when the root started, without the overlay
    /// cache, starting with the start fork. Like master's `top_frame_journal`, but for every fork
    /// and in plain mode too. Other forks are recorded when the root first selects them.
    root_start: Vec<(usize, StateSnapshot)>,
}

/// The placeholder left in the parent while the isolated child runs the inspector.
impl Default for Cheats {
    fn default() -> Self {
        Self {
            registry: Arc::default(),
            shared_overlay: None,
            selected: SavedState::default().rest,
            isolate: false,
            in_child: false,
            child_base: None,
            child_restores: Vec::new(),
            child_calls: Vec::new(),
            root_start: Vec::new(),
        }
    }
}

/// What a snapshot restore in an isolated child replaced.
struct ChildRestore {
    /// The fork active before the restore.
    fork: usize,
    state: StateSnapshot,
    selected: StateSnapshot,
    /// [`Cheats::child_base`] before the restore.
    base: Option<(usize, State<'static>)>,
}

/// A call running in an isolated child.
struct ChildCall {
    checkpoint: StateCheckpoint,
    /// Length of [`Cheats::child_restores`] when the call started.
    restores: usize,
}

impl Inspector<BaseEvmTypes> for Cheats {
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        // An isolated child's root frame has depth 0 too.
        if !self.in_child && message.depth == 0 {
            let start = SavedState::rest_of(interp.host().state_mut());
            self.root_start = vec![(self.registry.active, start)];
        }
        if message.destination == CHEATS {
            let output = self.dispatch(interp.host(), &message.input);
            return Some(message_result(message, output.is_some(), output.unwrap_or_default()));
        }
        if self.isolate && !self.in_child && message.depth == 1
            && message.kind == MessageKind::Call
        {
            return Some(self.isolated_call(interp.host(), message));
        }
        if self.in_child {
            let checkpoint = interp.host().state().checkpoint();
            self.child_calls.push(ChildCall { checkpoint, restores: self.child_restores.len() });
        }
        None
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        if self.in_child && message.destination != CHEATS {
            self.finish_child_call(interp.host(), result.is_success());
        }
        if !self.in_child && message.depth == 0 && !result.is_success() {
            self.restore_root(interp.host());
        }
    }
}

impl Cheats {
    fn dispatch(&mut self, evm: &mut Evm<'_, BaseEvmTypes>, input: &[u8]) -> Option<Bytes> {
        match input[0] {
            SELECT_FORK => {
                self.capture(evm);
                self.select_fork(evm, usize::from(input[1]));
                Some(Bytes::new())
            }
            SNAPSHOT => {
                self.capture(evm);
                let active_state = if self.child_base(self.registry.active).is_some() {
                    let mut state = evm.state().clone();
                    self.rebase_captured(self.registry.active, &mut state);
                    state.snapshot()
                } else {
                    evm.state().snapshot()
                };
                let snapshot = RegistrySnapshot {
                    active: self.registry.active,
                    active_state,
                    selected: self.selected.clone(),
                };
                self.registry_mut().snapshots.push(Arc::new(snapshot));
                Some(Bytes::new())
            }
            REVERT_TO => {
                self.capture(evm);
                if self.in_child {
                    self.child_restores.push(ChildRestore {
                        fork: self.registry.active,
                        state: evm.state().snapshot(),
                        selected: self.selected.clone(),
                        base: self.child_base.take(),
                    });
                }
                self.revert_to(evm, usize::from(input[1]));
                Some(Bytes::new())
            }
            TRANSACT => {
                self.capture(evm);
                let target = if input[1] == 0 { COUNTER } else { COLD_COUNTER };
                self.transact(evm, target)
            }
            STATE_WRITE => {
                evm.state_mut().storage_slot(&COUNTER, Word::ZERO).ok()?.set(Word::from(100));
                Some(Bytes::new())
            }
            MAKE_PERSISTENT => {
                self.registry_mut().persistent.insert(Address::with_last_byte(input[1]));
                Some(Bytes::new())
            }
            _ => None,
        }
    }

    /// Copies the accepted overlay that an [`Executor::call`] shares.
    ///
    /// Every operation that saves, replaces, or writes fork state calls this first, so the first
    /// call always happens while the start fork is still active. Operations that change only the
    /// registry don't call it; [`Self::registry_mut`] copies the registry alone.
    fn capture(&mut self, evm: &mut Evm<'_, BaseEvmTypes>) {
        if let Some(accepted) = self.shared_overlay.take() {
            // A snapshot or a saved fork keeps only the overlay cache and reloads it over the
            // backing database, which would skip the shared overlay.
            let backing = self.registry.backing(self.registry.active);
            let overlay = evm.overlay_db_mut();
            let reads = mem::replace(&mut overlay.cache, Cache::clone(&accepted));
            overlay.cache.merge(reads);
            overlay.db = Box::new(backing);
        }
    }

    /// Saves the active fork and loads `fork` into the running `Evm`.
    ///
    /// Persistent accounts follow the switch as in Foundry's `select_fork`: slots the incoming
    /// fork's transaction already loaded are refreshed, then `merge_account_data` brings the
    /// accepted overlay entry and the transaction layer. Like the fork's `journaled_state` in
    /// Foundry, [`Self::selected`] is recorded after the merge.
    fn select_fork(&mut self, evm: &mut Evm<'_, BaseEvmTypes>, fork: usize) {
        if self.registry.active == fork {
            return;
        }
        self.record_root_start(fork);
        let incoming =
            self.registry_mut().forks[fork].saved.take().expect("inactive fork is saved");
        let mut outgoing = replace_state(evm, incoming.load(self.registry.backing(fork)));
        self.rebase_captured(self.registry.active, &mut outgoing);
        for address in &self.registry.persistent {
            refresh_loaded_slots(evm.state_mut(), &outgoing.overlay_db().cache, address);
            merge_accepted_account(
                &mut evm.overlay_db_mut().cache,
                &outgoing.overlay_db().cache,
                address,
            );
            evm.state_mut().merge_transaction_account_from(address, &outgoing);
        }
        self.selected = SavedState::rest_of(evm.state_mut());
        let registry = self.registry_mut();
        let active = mem::replace(&mut registry.active, fork);
        registry.forks[active].saved = Some(SavedState::save(outgoing));
    }

    /// Restores the fork the snapshot was taken on, persistent accounts included, and leaves
    /// the other forks alone, as Foundry's `revert_state` does.
    ///
    /// If another fork is active, it keeps its accepted overlay and gets back the transaction
    /// layer it was selected with, as Foundry keeps that fork's database and `journaled_state`.
    /// Only its writes since it was selected are dropped.
    fn revert_to(&mut self, evm: &mut Evm<'_, BaseEvmTypes>, id: usize) {
        let RegistrySnapshot { active, active_state, selected } =
            RegistrySnapshot::clone(&self.registry.snapshots[id]);
        if active != self.registry.active {
            self.record_root_start(active);
        }
        let restored = active_state.into_state(self.registry.backing(active));
        let mut left = replace_state(evm, restored);
        let left_selected = mem::replace(&mut self.selected, selected);
        let registry = self.registry_mut();
        let left_fork = mem::replace(&mut registry.active, active);
        if left_fork != active {
            let cache = mem::take(&mut left.overlay_db_mut().cache);
            registry.forks[left_fork].saved =
                Some(SavedState { cache: Arc::new(cache), rest: left_selected });
            registry.forks[active].saved = None;
        }
    }

    /// Runs a nested transaction over the moved overlay and publishes it mid-transaction.
    ///
    /// Execution is the only fallible step. Publishing writes the accepted overlay and refreshes
    /// the live transaction layer from the child's pending state, without journaling and without
    /// further reads.
    fn transact(&mut self, evm: &mut Evm<'_, BaseEvmTypes>, target: Address) -> Option<Bytes> {
        let (output, pending) = run_child(evm, CALLER, target, Bytes::new(), 1_000_000, U256::ZERO).ok()?;
        evm.overlay_db_mut().commit_pending(&pending);
        evm.state_mut().merge_isolated_state(pending);
        Some(output)
    }

    /// Runs the call as a transaction in a child [`Evm`] that runs this inspector too, so
    /// cheatcodes called inside the child are dispatched, then folds the child's state back in
    /// like master's `transact_inner`.
    fn isolated_call(
        &mut self,
        evm: &mut Evm<'_, BaseEvmTypes>,
        message: &Message<BaseEvmTypes>,
    ) -> MessageResult<BaseEvmTypes> {
        let source_fork = self.registry.active;
        let cheats = Rc::new(RefCell::new(Self { in_child: true, ..mem::take(self) }));
        let result = run_child_with(evm, |parent, child| {
            // The child's intrinsic gas comes on top of the frame's gas, as in `transact_inner`.
            let to = TxKind::Call(message.destination);
            let intrinsic = intrinsic_gas(
                parent.version(),
                message.caller,
                to,
                &message.input,
                0,
                0,
                U256::ZERO,
            );
            let tx = legacy_tx(
                message.caller,
                message.destination,
                message.input.clone(),
                message.gas_limit + intrinsic,
                message.value,
            );
            cheats.borrow_mut().child_base =
                Some((source_fork, parent.state().clone_with(EmptyDB::default())));
            child.set_inspector(ChildCheats(Rc::clone(&cheats)));
            child.transact(&tx).map(|executed| executed.detach())
        });
        let cheats = Rc::into_inner(cheats).expect("the child is dropped").into_inner();
        *self = Self { in_child: false, child_base: None, ..cheats };
        let restored = !mem::take(&mut self.child_restores).is_empty();
        self.child_calls.clear();
        match result {
            Ok(out) => {
                // The child's logs follow the parent's, as if the call weren't isolated.
                evm.state_mut().logs_mut().extend(out.result.logs);
                let success = out.result.status;
                if self.registry.active != source_fork || (success && restored) {
                    // The child's state replaces the parent's after a fork switch, as in master.
                    // After a snapshot restore it also drops what the child no longer has, like
                    // master's `merge_child_state(.., true)`.
                    evm.state_mut().set_pending_state(out.pending_state);
                } else {
                    evm.state_mut().merge_isolated_state(out.pending_state);
                }
                // The frame pays the child's gas, intrinsic included, and takes its refund, as in
                // `transact_inner`. If that exceeds the frame's gas, the frame spends all of it.
                // `SPEC` has no state gas or calldata floor.
                let mut result = message_result(message, success, out.result.output);
                if result.gas.spend(out.result.total_gas_spent).is_err() {
                    result.gas.spend_all();
                }
                result.gas.set_refunded(out.result.refunded as i64);
                result
            }
            Err(_) => message_result(message, false, Bytes::new()),
        }
    }

    /// Undoes the snapshot restores of a failed call in the child, like master's
    /// `finish_isolated_snapshot_frame`, so they don't escape the call.
    ///
    /// evm2 already rolled the call back, but on the restored journal, which the call's checkpoint
    /// doesn't index. Put back the state from before the call's first restore and roll that back.
    /// Like fork selections, restores that end on another fork are not undone.
    fn finish_child_call(&mut self, evm: &mut Evm<'_, BaseEvmTypes>, success: bool) {
        let call = self.child_calls.pop().expect("the call started in the child");
        if success || self.child_restores.len() == call.restores {
            return;
        }
        let ChildRestore { fork, state, selected, base } =
            self.child_restores.drain(call.restores..).next().unwrap();
        if fork != self.registry.active {
            return;
        }
        let mut state = state.into_state(self.registry.backing(fork));
        state.rollback(call.checkpoint, evm.version().features);
        *evm.state_mut() = state;
        self.selected = selected;
        if base.is_some() {
            self.child_base = base;
        }
    }

    /// Records the state of the inactive `fork` before the root frame first changes it.
    fn record_root_start(&mut self, fork: usize) {
        if self.root_start.iter().any(|(recorded, _)| *recorded == fork) {
            return;
        }
        let saved = self.registry.forks[fork].saved.as_ref().expect("inactive fork is saved");
        self.root_start.push((fork, saved.rest.clone()));
    }

    /// Puts back the state that each fork the failed root frame touched had when the root started,
    /// keeping their accepted overlays, like master's `top_level_frame_end`.
    ///
    /// evm2 rolled back only the live fork's journal, which doesn't record the isolated children's
    /// writes, and a fork the root left was saved with its writes. Fork selections and snapshot
    /// restores stay in effect, as master's backend keeps them. If the root ends on another fork,
    /// it gets the persistent accounts as they were when the root started, as a switch then would
    /// have given it.
    fn restore_root(&mut self, evm: &mut Evm<'_, BaseEvmTypes>) {
        let root_start = mem::take(&mut self.root_start);
        let active = self.registry.active;
        let (start_fork, start) = &root_start[0];
        let start = (active != *start_fork).then(|| start.clone().into_state(EmptyDB::default()));
        for (fork, rest) in root_start {
            if fork == active {
                let mut state = rest.into_state(EmptyDB::default());
                mem::swap(state.overlay_db_mut(), evm.overlay_db_mut());
                *evm.state_mut() = state;
            } else {
                self.registry_mut().forks[fork]
                    .saved
                    .as_mut()
                    .expect("inactive fork is saved")
                    .rest = rest;
            }
        }
        if let Some(start) = start {
            for address in &self.registry.persistent {
                evm.state_mut().merge_transaction_account_from(address, &start);
            }
        }
    }

    /// The parent's state, while an isolated child that started on `fork` runs.
    fn child_base(&self, fork: usize) -> Option<&State<'static>> {
        self.child_base.as_ref().filter(|(base_fork, _)| *base_fork == fork).map(|(_, base)| base)
    }

    /// Gives `state`, captured on `fork` in an isolated child, the parent's originals back.
    fn rebase_captured(&self, fork: usize, state: &mut State<'_>) {
        if let Some(base) = self.child_base(fork) {
            rebase_isolated_originals(state, base);
        }
    }

    /// The registry, copied on write like `CowBackend::backend_mut` if an [`Executor::call`]
    /// shares it. Snapshots stay shared.
    fn registry_mut(&mut self) -> &mut Registry {
        Arc::make_mut(&mut self.registry)
    }
}

/// Runs the parent's [`Cheats`] in an isolated child. [`Evm::clear_inspector_as`] needs a
/// `'static` [`Evm`], so the parent takes the inspector back through the shared cell.
struct ChildCheats(Rc<RefCell<Cheats>>);

impl Inspector<BaseEvmTypes> for ChildCheats {
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.0.borrow_mut().call(interp, message)
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.0.borrow_mut().call_end(interp, message, result);
    }
}

/// Runs `f` with a child [`Evm`] that owns the parent's overlay, then moves the overlay back.
///
/// The child's transaction layer starts from the parent's via `prepare_isolated_state`.
fn run_child_with<'a, R>(
    parent: &mut Evm<'a, BaseEvmTypes>,
    f: impl FnOnce(&Evm<'a, BaseEvmTypes>, &mut Evm<'a, BaseEvmTypes>) -> R,
) -> R {
    let mut child = new_evm(EmptyDB::default());
    mem::swap(child.overlay_db_mut(), parent.overlay_db_mut());
    child.state_mut().set_pending_state(parent.state().prepare_isolated_state());
    let result = f(parent, &mut child);
    mem::swap(child.overlay_db_mut(), parent.overlay_db_mut());
    result
}

fn run_child(
    parent: &mut Evm<'_, BaseEvmTypes>,
    caller: Address,
    to: Address,
    input: Bytes,
    gas_limit: u64,
    value: U256,
) -> Result<(Bytes, PendingState), ()> {
    run_child_with(parent, |_, child| {
        let out = child.transact(&legacy_tx(caller, to, input, gas_limit, value)).map_err(drop)?.detach();
        if out.result.status { Ok((out.result.output, out.pending_state)) } else { Err(()) }
    })
}

/// Merges the accepted entry of `address` in `source`, if cached, into `target`, giving it
/// precedence as [`Cache::merge`] does: account, code, and storage, including a wipe.
///
/// [`State::merge_transaction_account_from`] alone is not enough. It skips accounts not loaded in
/// this transaction, and the original values it copies come from `source`. A commit only accepts
/// entries that differ from their original, so an entry that is loaded but unchanged would fall
/// back to `target`'s own state once the transaction ends.
fn merge_accepted_account(target: &mut Cache, source: &Cache, address: &Address) {
    let mut entry = Cache::default();
    if let Some(account) = source.accounts.get(address) {
        if let Some(info) = account
            && let Some(code) = source.contracts.get(&info.code_hash)
        {
            entry.contracts.insert(info.code_hash, code.clone());
        }
        entry.accounts.insert(*address, account.clone());
    }
    if let Some(storage) = source.storage.get(address) {
        entry.storage.insert(*address, storage.clone());
    }
    target.merge(entry);
}

/// Gives each slot of `address` that `target`'s transaction has loaded its accepted value in
/// `source`, if cached, as Foundry's `select_fork` refreshes the target fork's journaled state
/// (foundry-rs/foundry#10296, #10552).
///
/// The transaction layer shadows the accepted overlay, so merging the accepted entry alone leaves
/// such a slot with the incoming fork's value. Like Foundry, this replaces only the current value:
/// the slot keeps its original and warmth, and nothing is journaled. `storage_slot` doesn't load
/// the account, and `merge_isolated_state` keeps the originals of loaded slots.
fn refresh_loaded_slots(target: &mut State<'_>, source: &Cache, address: &Address) {
    let Some(storage) = source.storage.get(address) else { return };
    let mut refreshed = State::new(EmptyDB::default());
    for (key, value) in &storage.slots {
        if target.get_storage(address, key).is_some() {
            refreshed.storage_slot(address, *key).unwrap().set(*value);
        }
    }
    target.merge_isolated_state(refreshed.prepare_isolated_state());
}

/// Gives every account and slot that `state`, captured in an isolated child, shares with `base`,
/// the parent state the child was prepared from, its original in `base`, through public API.
///
/// `prepare_isolated_state` made the child's originals the parent's current values, which differ
/// only for entries the parent's transaction changed. Each account the parent wrote takes the
/// parent's whole entry, then the captured current values, flags, and the child's new entries go
/// back on top. The parent's entries are re-added if `state` dropped them, so this must not run on
/// state a snapshot restore replaced. Accounts the parent wrote also keep the parent's warmth
/// instead of the child's.
fn rebase_isolated_originals(state: &mut State<'_>, base: &State<'_>) {
    let captured = state.prepare_isolated_state();
    let writes = transaction_writes(base);
    let storage_only =
        writes.storage.keys().filter(|address| !writes.accounts.contains_key(*address));
    for address in writes.accounts.keys().chain(storage_only) {
        state.merge_transaction_account_from(address, base);
    }
    state.merge_isolated_state(captured);
}

/// Replaces the live state of `evm` with `state`, which keeps the transaction's logs.
///
/// Logs belong to the transaction, not to a fork or a snapshot: master keeps one journal across
/// fork switches, and a snapshot restore takes the current logs (`BackendStateSnapshot::merge`).
/// A loaded fork or a restored snapshot would bring back the logs it was captured with instead.
/// The returned state has none.
fn replace_state<'a>(evm: &mut Evm<'a, BaseEvmTypes>, mut state: State<'a>) -> State<'a> {
    *state.logs_mut() = mem::take(evm.state_mut().logs_mut());
    mem::replace(evm.state_mut(), state)
}

/// The accepted-state changes that committing `state`'s transaction layer would make.
fn transaction_writes(state: &State<'_>) -> Cache {
    let mut state = state.clone();
    state.overlay_db_mut().cache = Cache::default();
    state.commit_transaction();
    mem::take(&mut state.overlay_db_mut().cache)
}

/// Foundry-style executor: owns the registry and the active fork's state between calls.
#[derive(Clone)]
struct Executor {
    /// Shared with a running [`Executor::call`].
    registry: Arc<Registry>,
    /// The active fork's state while no call runs.
    active: Option<SavedState>,
    isolate: bool,
}

impl Executor {
    fn new(backings: Vec<BackingDb>) -> Self {
        // Like Foundry's cheatcode account, which has code `0x00` and, as a default persistent
        // account, exists on every fork.
        let cheats =
            AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from_static(&[op::STOP])));
        let forks = backings
            .into_iter()
            .map(|backing| {
                let mut state = State::new(EmptyDB::default());
                state.overlay_db_mut().insert_account_info(&CHEATS, cheats.clone());
                ForkEntry { backing, saved: Some(SavedState::save(state)) }
            })
            .collect();
        let mut registry = Registry { forks, ..Default::default() };
        registry.persistent.extend([CALLER, TEST]);
        let active = registry.forks[0].saved.take();
        Self { registry: Arc::new(registry), active, isolate: false }
    }

    /// Writes outside a transaction go straight to the active overlay.
    fn set_code(&mut self, address: Address, code: Bytecode) {
        let mut state = self.active.take().unwrap().load(EmptyDB::default());
        state
            .overlay_db_mut()
            .insert_account_info(&address, AccountInfo::default().with_code(code));
        self.active = Some(SavedState::save(state));
    }

    /// Calls the test contract running `script` and commits.
    fn run(&mut self, script: &[(Address, &[u8])]) {
        let result = self.run_code(script_code(script));
        assert!(result.status, "test call failed: {result:?}");
    }

    /// Calls the test contract with `code`, which may fail, and commits.
    fn run_code(&mut self, code: Bytecode) -> TxResult<BaseEvmTypes> {
        self.set_code(TEST, code);
        let active = self.registry.active;
        let mut evm = new_evm(EmptyDB::default());
        let state = self.active.take().unwrap();
        // The previous call accepted or dropped the active fork's transaction layer, so the call
        // start counts as its selection.
        let selected = state.rest.clone();
        *evm.state_mut() = state.load(self.registry.backing(active));
        evm.set_inspector(Cheats {
            registry: mem::take(&mut self.registry),
            selected,
            isolate: self.isolate,
            ..Default::default()
        });
        let executed = evm.transact(&legacy_tx(CALLER, TEST, Bytes::new(), 10_000_000, U256::ZERO)).unwrap();
        let result = executed.commit();
        let cheats = *evm.clear_inspector_as::<Cheats>().unwrap();
        let state = mem::replace(evm.state_mut(), State::new(EmptyDB::default()));
        self.finish(cheats, state);
        result
    }

    fn finish(&mut self, cheats: Cheats, state: State<'_>) {
        let mut registry = cheats.registry;
        // `ExecutedTx::commit` only finalized and accepted the active fork; do the inactive ones
        // too.
        for fork in &mut Arc::make_mut(&mut registry).forks {
            fork.saved = fork.saved.take().map(SavedState::accept_transaction);
        }
        self.active = Some(SavedState::save(state));
        self.registry = registry;
    }

    /// Calls the test contract that [`Self::set_code`] installed and discards the transaction
    /// without touching the executor, like master's `Executor::call` over
    /// `CowBackend::new_borrowed`.
    ///
    /// The call shares the registry and the active fork's accepted overlay with the executor
    /// rather than borrowing them. Inspector hooks are generic over the [`Evm`]'s lifetime, so a
    /// borrowing [`Cheats`] couldn't install itself in an isolated child. Neither is copied unless
    /// a cheatcode mutates. Returns the output and the inspector, as master's `RawCallResult`
    /// carries the cheatcodes.
    fn call(&self) -> (Bytes, Cheats) {
        let active = self.registry.active;
        let state = self.active.as_ref().unwrap();
        let view = AcceptedView {
            cache: Arc::clone(&state.cache),
            backing: self.registry.backing(active),
        };
        let mut evm = new_evm(EmptyDB::default());
        *evm.state_mut() = state.rest.clone().into_state(view);
        evm.set_inspector(Cheats {
            registry: Arc::clone(&self.registry),
            shared_overlay: Some(Arc::clone(&state.cache)),
            selected: state.rest.clone(),
            isolate: self.isolate,
            ..Default::default()
        });
        let executed = evm.transact(&legacy_tx(CALLER, TEST, Bytes::new(), 10_000_000, U256::ZERO)).unwrap();
        let result = executed.discard();
        assert!(result.status, "test call failed: {result:?}");
        (result.output, *evm.clear_inspector_as::<Cheats>().unwrap())
    }

    /// Reads a slot of `fork` the way executor helpers do: through the overlay and saved caches.
    fn storage(&self, fork: usize, address: Address, key: Word) -> Word {
        self.saved(fork).storage(self.registry.backing(fork), address, key)
    }

    /// The accepted overlay's cached value of a slot, if any.
    fn accepted(&self, fork: usize, address: Address, key: Word) -> Option<Word> {
        self.saved(fork).cache.storage.get(&address)?.slots.get(&key).copied()
    }

    /// Reads an account of `fork` like [`Self::storage`].
    fn account(&self, fork: usize, address: Address) -> Option<AccountInfo> {
        self.saved(fork).account(self.registry.backing(fork), address)
    }

    /// The saved state of `fork`, which is [`Self::active`] for the active fork.
    fn saved(&self, fork: usize) -> &SavedState {
        let saved = if fork == self.registry.active {
            self.active.as_ref()
        } else {
            self.registry.forks[fork].saved.as_ref()
        };
        saved.unwrap()
    }
}

fn execution_config() -> ExecutionConfig<BaseEvmTypes> {
    let mut version = Version::new(SPEC);
    // Foundry disables these for test execution.
    version.features.remove(
        EvmFeatures::NONCE_CHECK | EvmFeatures::EIP3607 | EvmFeatures::BLOCK_GAS_LIMIT_CHECK,
    );
    ExecutionConfig::for_spec_and_version(SPEC, version)
}

fn new_evm<'a>(db: impl DynDatabase + 'a) -> Evm<'a, BaseEvmTypes> {
    Evm::new_with_execution_config(
        execution_config(),
        SPEC,
        BlockEnvExt::default(),
        ethereum_tx_registry(SPEC),
        db,
        Precompiles::base(SPEC),
    )
}

fn legacy_tx(
    caller: Address,
    to: Address,
    input: Bytes,
    gas_limit: u64,
    value: U256,
) -> Recovered<TxEnvelope> {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(to),
            input,
            gas_limit,
            value,
            ..Default::default()
        }),
        caller,
    )
}

fn message_result(
    message: &Message<BaseEvmTypes>,
    success: bool,
    output: Bytes,
) -> MessageResult<BaseEvmTypes> {
    MessageResult::<BaseEvmTypes> {
        stop: if success { InstrStop::Return } else { InstrStop::Revert },
        gas: GasTracker::new(message.gas_limit),
        output,
        created_address: None,
        ext: Default::default(),
        _non_exhaustive: (),
    }
}

/// Increments slot 0 and returns the new value.
fn counter_code() -> Bytecode {
    Bytecode::new_legacy(Bytes::from_static(&[
        op::PUSH0,
        op::SLOAD,
        op::PUSH1,
        1,
        op::ADD,
        op::DUP1,
        op::PUSH0,
        op::SSTORE,
        op::PUSH0,
        op::MSTORE,
        op::PUSH1,
        32,
        op::PUSH0,
        op::RETURN,
    ]))
}

/// Code that emits its calldata as a `LOG0`.
fn logger_code() -> Bytecode {
    Bytecode::new_legacy(Bytes::from_static(&[
        op::CALLDATASIZE,
        op::PUSH0,
        op::PUSH0,
        op::CALLDATACOPY,
        op::CALLDATASIZE,
        op::PUSH0,
        op::LOG0,
        op::STOP,
    ]))
}

/// Code that creates a contract whose initcode writes its slot 0 = 1 and self-destructs to the
/// creator.
fn factory_code() -> Bytecode {
    let init = [op::PUSH1, 1, op::PUSH0, op::SSTORE, op::CALLER, op::SELFDESTRUCT];
    let mut code = vec![op::PUSH6];
    code.extend_from_slice(&init);
    code.extend([op::PUSH0, op::MSTORE]);
    // CREATE(value, offset, size) of the initcode, right-aligned in the first memory word.
    code.extend([op::PUSH1, init.len() as u8, op::PUSH1, 32 - init.len() as u8, op::PUSH0]);
    code.extend([op::CREATE, op::POP, op::STOP]);
    Bytecode::new_legacy(code.into())
}

/// Code that performs `calls` in order and ignores their results.
fn script_code(calls: &[(Address, &[u8])]) -> Bytecode {
    script_code_ending(calls, &[op::STOP])
}

/// Like [`script_code`], then reverts.
fn reverting_script_code(calls: &[(Address, &[u8])]) -> Bytecode {
    script_code_ending(calls, &[op::PUSH0, op::PUSH0, op::REVERT])
}

/// Like [`script_code`], then returns what the last call returned.
fn returning_script_code(calls: &[(Address, &[u8])]) -> Bytecode {
    script_code_ending(
        calls,
        &[
            op::RETURNDATASIZE,
            op::PUSH0,
            op::PUSH0,
            op::RETURNDATACOPY,
            op::RETURNDATASIZE,
            op::PUSH0,
            op::RETURN,
        ],
    )
}

fn script_code_ending(calls: &[(Address, &[u8])], end: &[u8]) -> Bytecode {
    let mut code = Vec::new();
    for (to, data) in calls {
        let mut word = [0; 32];
        word[..data.len()].copy_from_slice(data);
        code.push(op::PUSH32);
        code.extend_from_slice(&word);
        code.extend([op::PUSH0, op::MSTORE]);
        // CALL(gas, to, value, argsOffset, argsSize, retOffset, retSize), pushed in reverse.
        code.extend([op::PUSH0, op::PUSH0, op::PUSH1, data.len() as u8, op::PUSH0, op::PUSH0]);
        code.push(op::PUSH20);
        code.extend_from_slice(to.as_slice());
        code.extend([op::GAS, op::CALL, op::POP]);
    }
    code.extend_from_slice(end);
    Bytecode::new_legacy(code.into())
}

/// An executor over two forks, with counters at 10 and 20, and `contracts` on the first.
fn executor_with(contracts: &[(Address, Bytecode)]) -> Executor {
    let mut executor =
        Executor::new(vec![BackingDb::with_counter(10), BackingDb::with_counter(20)]);
    for (address, code) in contracts {
        executor.set_code(*address, code.clone());
    }
    executor
}

/// Fork 0's counter after `script` runs plainly or isolated.
fn counter_after(
    contracts: &[(Address, Bytecode)],
    script: &[(Address, &[u8])],
    isolate: bool,
) -> u64 {
    let mut executor = executor_with(contracts);
    executor.isolate = isolate;
    executor.run(script);
    counter(&executor, 0)
}

/// Runs `script` isolated with `handler` at [`HANDLER`], and returns the root's gas.
fn run_isolated(handler: Bytecode, script: &[(Address, &[u8])]) -> (Executor, u64) {
    let mut executor = executor_with(&[(HANDLER, handler)]);
    executor.isolate = true;
    let result = executor.run_code(script_code(script));
    assert!(result.status, "test call failed: {result:?}");
    (executor, result.total_gas_spent)
}

fn counter(executor: &Executor, fork: usize) -> u64 {
    executor.storage(fork, COUNTER, Word::ZERO).to::<u64>()
}

/// The caller's nonce on the active fork.
fn nonce(executor: &Executor) -> u64 {
    let active = executor.registry.active;
    let mut state = executor.active.clone().unwrap().load(executor.registry.backing(active));
    state.account_info_untracked(&CALLER).unwrap().unwrap().nonce
}

const INC: (Address, &[u8]) = (COUNTER, &[]);

const PERSIST_COUNTER: (Address, &[u8]) = (CHEATS, &[MAKE_PERSISTENT, COUNTER.0.0[19]]);

const fn select(fork: u8) -> (Address, &'static [u8]) {
    match fork {
        0 => (CHEATS, &[SELECT_FORK, 0]),
        _ => (CHEATS, &[SELECT_FORK, 1]),
    }
}

/// A call to [`LOGGER`] that emits a log with data `[n]`.
fn emit(n: u8) -> (Address, &'static [u8]) {
    let data: &'static [u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    (LOGGER, &data[usize::from(n)..][..1])
}

/// The data of the logs that a committed call of `code` returns, run plainly or isolated.
fn logs_after(executor: &mut Executor, code: Bytecode, isolate: bool) -> Vec<u8> {
    executor.isolate = isolate;
    let result = executor.run_code(code);
    result.logs.iter().map(|log| log.data.data[0]).collect()
}

/// An executor with `contracts` on the first fork and [`LOGGER`] persistent, like the test
/// contract that emits a test's events.
fn logging_executor(contracts: &[(Address, Bytecode)]) -> Executor {
    let mut executor = executor_with(contracts);
    executor.set_code(LOGGER, logger_code());
    Arc::make_mut(&mut executor.registry).persistent.insert(LOGGER);
    executor
}

#[test]
fn commit_on_active_fork() {
    let backing = BackingDb::with_counter(10);
    let mut executor = Executor::new(vec![backing.clone()]);

    executor.run(&[INC]);
    assert_eq!(executor.accepted(0, COUNTER, Word::ZERO), Some(Word::from(11)));
    let reads = backing.reads();

    executor.run(&[INC, INC]);
    assert_eq!(counter(&executor, 0), 13);
    // The overlay serves the second transaction: the counter is not fetched again.
    assert_eq!(backing.reads(), reads);

    executor.set_code(TEST, script_code(&[INC]));
    executor.call();
    assert_eq!(counter(&executor, 0), 13);
    // Commits never write the backing database.
    assert_eq!(backing.0.storage[&(COUNTER, Word::ZERO)], Word::from(10));
}

#[test]
fn save_and_reload_fork_with_pending_writes_across_transactions() {
    let a = BackingDb::with_counter(10);
    let mut executor = Executor::new(vec![a.clone(), BackingDb::with_counter(20)]);

    // Write on A, switch to B mid-transaction, write on B, commit while B is active.
    executor.run(&[INC, select(1), INC]);
    assert_eq!(executor.registry.active, 1);
    assert_eq!(executor.accepted(1, COUNTER, Word::ZERO), Some(Word::from(21)));
    // The commit also accepts A's write into A's own overlay, though A is inactive.
    assert_eq!(executor.accepted(0, COUNTER, Word::ZERO), Some(Word::from(11)));
    assert_eq!(counter(&executor, 0), 11);
    // Its per-transaction substate was cleared at the boundary.
    let saved = executor.registry.forks[0].saved.clone().unwrap().load(EmptyDB::default());
    assert!(saved.journal().is_empty());

    // A later transaction reloads A with its accepted write and leaves it inactive again.
    let reads = a.reads();
    executor.run(&[select(0), INC, select(1), INC]);
    assert_eq!((counter(&executor, 0), counter(&executor, 1)), (12, 22));
    assert_eq!(a.reads(), reads, "reloading A must reuse its moved cache");

    // Committing while A is active accepts its write as usual.
    executor.run(&[select(0), INC]);
    assert_eq!(executor.accepted(0, COUNTER, Word::ZERO), Some(Word::from(13)));
    assert_eq!((counter(&executor, 0), counter(&executor, 1)), (13, 22));
}

/// Writes on a fork that is inactive at commit must not depend on being repeated once it is
/// reselected, as they would if they stayed pending in its transaction layer.
#[test]
fn reselected_fork_keeps_writes_it_does_not_repeat() {
    let counters = |executor: &Executor| {
        (counter(executor, 0), executor.storage(0, COLD_COUNTER, Word::ZERO).to::<u64>())
    };
    let mut executor =
        Executor::new(vec![BackingDb::with_counter(10), BackingDb::with_counter(20)]);
    // Write two slots on A, then commit while B is active.
    executor.run(&[INC, (COLD_COUNTER, &[]), select(1)]);

    // Committing on A without writing keeps both.
    let mut reselected = executor.clone();
    reselected.run(&[select(0)]);
    assert_eq!(reselected.registry.active, 0);
    assert_eq!(counters(&reselected), (11, 11));

    // Rewriting one keeps the other.
    executor.run(&[select(0), INC]);
    assert_eq!(counters(&executor), (12, 11));
}

/// A persistent account follows a fork switch even when the transaction hasn't loaded it, as when
/// `setUp` committed it.
#[test]
fn persistent_account_follows_fork_switch() {
    let mut executor =
        Executor::new(vec![BackingDb::with_counter(10), BackingDb::with_counter(20)]);
    Arc::make_mut(&mut executor.registry).persistent.insert(COUNTER);
    executor.run(&[INC]);

    executor.run(&[select(1), INC]);
    assert_eq!(counter(&executor, 1), 12);
    executor.run(&[select(0), INC]);
    assert_eq!(counter(&executor, 0), 13);
}

/// A persistent account that the transaction loads but leaves unchanged keeps its state on the
/// fork it was switched to, even though the commit doesn't accept it there.
#[test]
fn persistent_account_unchanged_by_transaction_survives_commit() {
    let deployed = |executor: &Executor| executor.storage(1, DEPLOYED, Word::ZERO).to::<u64>();
    let mut executor =
        Executor::new(vec![BackingDb::with_counter(10), BackingDb::with_counter(20)]);
    executor.set_code(DEPLOYED, counter_code());
    Arc::make_mut(&mut executor.registry).persistent.insert(DEPLOYED);

    // Both calls change the slot, but none changes the account with the code.
    executor.run(&[(DEPLOYED, &[]), select(1), (DEPLOYED, &[])]);
    assert_eq!(deployed(&executor), 2);
    executor.run(&[(DEPLOYED, &[])]);
    assert_eq!(deployed(&executor), 3);
}

/// A slot of a persistent account that the incoming fork's transaction already loaded takes the
/// outgoing fork's accepted value, as Foundry refreshes it. The forked counters are 11 on A and
/// 21 on B when the counter is made persistent on A; Foundry gives 12 on B after an increment and
/// 11 without one, in plain and isolated mode alike.
#[test]
fn persistent_account_refreshes_slot_loaded_on_incoming_fork() {
    let scripts = [
        (&[select(1), INC, select(0), PERSIST_COUNTER, select(1), INC][..], 12),
        (&[select(1), INC, select(0), PERSIST_COUNTER, select(1)], 11),
    ];
    for (script, expected) in scripts {
        for isolate in [false, true] {
            let mut executor = executor_with(&[]);
            executor.run(&[INC]);
            executor.isolate = isolate;
            executor.run(script);
            assert_eq!(executor.registry.active, 1, "isolate: {isolate}");
            assert_eq!(counter(&executor, 1), expected, "isolate: {isolate}");
            assert_eq!(counter(&executor, 0), 11, "isolate: {isolate}");
        }
    }
}

/// Like Foundry's `revert_state`, a snapshot restore also reverts persistent accounts.
#[test]
fn snapshot_restore_reverts_persistent_accounts() {
    let mut executor = Executor::new(vec![BackingDb::with_counter(10)]);
    Arc::make_mut(&mut executor.registry).persistent.insert(COUNTER);
    executor.run(&[(CHEATS, &[SNAPSHOT]), INC, (CHEATS, &[REVERT_TO, 0])]);
    assert_eq!(counter(&executor, 0), 10);
}

/// Like Foundry's, a snapshot restore replaces only the fork the snapshot was taken on. Other
/// forks keep their committed and pending writes.
#[test]
fn snapshot_restore_leaves_other_forks() {
    let new = || Executor::new(vec![BackingDb::with_counter(10), BackingDb::with_counter(20)]);
    let counters = |executor: &Executor| (counter(executor, 0), counter(executor, 1));

    // B's write was committed while B was active, and B is still active at the restore.
    let mut executor = new();
    executor.run(&[(CHEATS, &[SNAPSHOT])]);
    executor.run(&[INC, select(1), INC]);
    executor.run(&[(CHEATS, &[REVERT_TO, 0])]);
    assert_eq!(executor.registry.active, 0);
    assert_eq!(counters(&executor), (10, 21));

    // B's write was committed while B was inactive.
    let mut executor = new();
    executor.run(&[(CHEATS, &[SNAPSHOT])]);
    executor.run(&[INC, select(1), INC, select(0)]);
    executor.run(&[(CHEATS, &[REVERT_TO, 0])]);
    assert_eq!(counters(&executor), (10, 21));

    // B's write is still pending in its saved state when A's snapshot is restored.
    let mut executor = new();
    let script: &[(Address, &[u8])] =
        &[(CHEATS, &[SNAPSHOT]), INC, select(1), INC, select(0), (CHEATS, &[REVERT_TO, 0])];
    executor.run(script);
    assert_eq!(counters(&executor), (10, 21));
}

/// A restore that leaves the active fork drops only what the fork wrote since it was selected, as
/// Foundry keeps that fork's database and the `journaled_state` it was selected with.
#[test]
fn snapshot_restore_drops_writes_since_left_fork_was_selected() {
    let new = || Executor::new(vec![BackingDb::with_counter(10), BackingDb::with_counter(20)]);

    // B was selected in this transaction.
    let mut executor = new();
    executor.run(&[(CHEATS, &[SNAPSHOT]), select(1), INC, (CHEATS, &[REVERT_TO, 0])]);
    assert_eq!(executor.registry.active, 0);
    assert_eq!(counter(&executor, 1), 20);

    // B was selected in an earlier call.
    let mut executor = new();
    executor.run(&[(CHEATS, &[SNAPSHOT]), select(1)]);
    executor.run(&[INC, (CHEATS, &[REVERT_TO, 0])]);
    assert_eq!(counter(&executor, 1), 20);

    // B's write from an earlier selection stays pending; only the one since it was reselected is
    // dropped.
    let mut executor = new();
    let script: &[(Address, &[u8])] = &[
        (CHEATS, &[SNAPSHOT]),
        select(1),
        INC,
        select(0),
        select(1),
        INC,
        (CHEATS, &[REVERT_TO, 0]),
    ];
    executor.run(script);
    assert_eq!(counter(&executor, 1), 21);

    // `vm.transact` writes B's overlay, as Foundry writes the fork's database, so it stays.
    let mut executor = new();
    let script: &[(Address, &[u8])] =
        &[(CHEATS, &[SNAPSHOT]), select(1), (CHEATS, &[TRANSACT, 0]), (CHEATS, &[REVERT_TO, 0])];
    executor.run(script);
    assert_eq!(counter(&executor, 1), 21);
}

#[test]
fn speculative_call_leaves_registry_after_fork_switch_and_snapshot_restore() {
    let script: &[(Address, &[u8])] = &[
        INC,
        select(1),
        INC,
        (CHEATS, &[SNAPSHOT]),
        INC,
        (CHEATS, &[REVERT_TO, 1]),
        select(0),
        INC,
    ];
    for isolate in [false, true] {
        let mut executor = executor_with(&[]);
        executor.isolate = isolate;
        executor.run(&[INC, (CHEATS, &[SNAPSHOT])]);
        assert_eq!(executor.registry.snapshots.len(), 1);

        // Control: committed, the script switches forks, snapshots, and restores as intended.
        let mut control = executor.clone();
        control.run(script);
        assert_eq!(control.registry.snapshots.len(), 2, "isolate: {isolate}");
        assert_eq!((counter(&control, 0), counter(&control, 1)), (13, 21), "isolate: {isolate}");

        executor.set_code(TEST, returning_script_code(script));
        let (output, cheats) = executor.call();
        assert_eq!(Word::from_be_slice(&output), Word::from(13), "isolate: {isolate}");
        assert_eq!(cheats.registry.snapshots.len(), 2, "isolate: {isolate}");
        drop(cheats);
        assert_eq!(executor.registry.active, 0, "isolate: {isolate}");
        assert_eq!(executor.registry.snapshots.len(), 1, "isolate: {isolate}");
        assert_eq!((counter(&executor, 0), counter(&executor, 1)), (11, 20), "isolate: {isolate}");
    }
}

/// Without a fork switch, an overlay write or a snapshot restore on the start fork still works on
/// the call's copy of the accepted overlay. The call sees setUp's write, its own write, and the
/// restored state, and the executor keeps none of them: `vm.transact` in one test doesn't leak
/// into the next, and restoring an older snapshot doesn't drop setUp's committed write.
#[test]
fn speculative_call_copies_shared_overlay_for_start_fork_write_or_restore() {
    let scripts: [&[(Address, &[u8])]; 3] = [
        &[(CHEATS, &[TRANSACT, 0]), INC],
        &[(CHEATS, &[REVERT_TO, 0]), INC],
        &[(CHEATS, &[TRANSACT, 0]), (CHEATS, &[REVERT_TO, 0]), INC],
    ];
    for (script, expected) in scripts.into_iter().zip([13, 11, 11]) {
        for isolate in [false, true] {
            let mut executor = executor_with(&[]);
            executor.isolate = isolate;
            executor.run(&[(CHEATS, &[SNAPSHOT])]);
            executor.run(&[INC]);
            let mut control = executor.clone();
            control.run(script);
            assert_eq!(counter(&control, 0), expected, "{script:?}, isolate: {isolate}");

            executor.set_code(TEST, returning_script_code(script));
            let (output, cheats) = executor.call();
            assert_eq!(
                Word::from_be_slice(&output),
                Word::from(expected),
                "{script:?}, isolate: {isolate}"
            );
            assert!(cheats.shared_overlay.is_none(), "{script:?}, isolate: {isolate}");
            drop(cheats);
            assert_eq!(executor.registry.snapshots.len(), 1, "{script:?}, isolate: {isolate}");
            assert_eq!(counter(&executor, 0), 11, "{script:?}, isolate: {isolate}");
        }
    }
}

#[test]
fn isolated_child_shares_overlay_by_move() {
    let backing = BackingDb::with_counter(10);
    let mut executor = Executor::new(vec![backing.clone()]);
    executor.run(&[INC]);
    executor.isolate = true;
    let reads = backing.reads();

    executor.run(&[INC, INC]);
    // The second child starts from the first child's merged write.
    assert_eq!(counter(&executor, 0), 13);
    assert_eq!(backing.reads(), reads, "children read the moved overlay, not the backing");
}

/// The parent frame pays an isolated child's gas, as master's `transact_inner` charges the frame
/// with the child's transaction. The child runs as its own transaction, so a call costs its
/// intrinsic gas more than a plain one.
#[test]
fn isolated_call_charges_child_gas() {
    let gas = [false, true].map(|isolate| {
        let mut executor = executor_with(&[]);
        executor.isolate = isolate;
        executor.run_code(script_code(&[INC])).total_gas_spent
    });
    assert_eq!(gas[1] - gas[0], 21_000, "plain: {}, isolated: {}", gas[0], gas[1]);
}

/// Cheatcodes that a contract calls inside an isolated child reach the inspector. A fork the child
/// selects stays selected in the parent, which adopts the child's state.
#[test]
fn isolated_child_dispatches_cheatcodes() {
    for isolate in [false, true] {
        let mut executor =
            executor_with(&[(HANDLER, script_code(&[(CHEATS, &[SNAPSHOT]), select(1), INC]))]);
        executor.isolate = isolate;

        executor.run(&[(HANDLER, &[]), INC]);
        assert_eq!(executor.registry.snapshots.len(), 1, "isolate: {isolate}");
        assert_eq!(executor.registry.active, 1, "isolate: {isolate}");
        assert_eq!((counter(&executor, 0), counter(&executor, 1)), (10, 22), "isolate: {isolate}");
    }
}

/// After a snapshot restore inside an isolated child, the parent drops its writes since the
/// snapshot, as master's `merge_child_state(.., remove_absent = true)` does. evm2's
/// `merge_isolated_state` has no such mode, but `set_pending_state` gives the same result: the
/// restored state's originals are the parent's own, from when it took the snapshot.
#[test]
fn isolated_child_restore_drops_parent_writes_since_snapshot() {
    for isolate in [false, true] {
        let mut executor = executor_with(&[(
            HANDLER,
            script_code(&[(CHEATS, &[REVERT_TO, 0]), (COLD_COUNTER, &[])]),
        )]);
        executor.isolate = isolate;

        executor.run(&[(CHEATS, &[SNAPSHOT]), INC, (HANDLER, &[])]);
        assert_eq!(counter(&executor, 0), 10, "isolate: {isolate}");
        assert_eq!(
            executor.storage(0, COLD_COUNTER, Word::ZERO),
            Word::from(11),
            "isolate: {isolate}"
        );
    }
}

/// A restore inside an isolated child that then reverts doesn't escape it, as in master's
/// `test_reverted_isolated_restore_does_not_escape`.
#[test]
fn reverted_restore_in_isolated_child_does_not_escape() {
    let mut executor =
        executor_with(&[(FAILING, reverting_script_code(&[INC, (CHEATS, &[REVERT_TO, 0])]))]);
    executor.isolate = true;

    executor.run(&[INC, (CHEATS, &[SNAPSHOT]), INC, (FAILING, &[])]);
    assert_eq!(counter(&executor, 0), 12);
}

/// A reverted call inside an isolated child takes its restore with it, and the child goes on from
/// the state before the call, as in master's `test_caught_nested_restore_revert_does_not_escape`.
#[test]
fn caught_reverted_restore_in_isolated_child_is_undone() {
    let mut executor = executor_with(&[
        (HANDLER, script_code(&[(FAILING, &[]), INC])),
        (FAILING, reverting_script_code(&[INC, (CHEATS, &[REVERT_TO, 0])])),
    ]);
    executor.isolate = true;

    executor.run(&[INC, (CHEATS, &[SNAPSHOT]), INC, (HANDLER, &[])]);
    assert_eq!(counter(&executor, 0), 13);
}

/// A reverted call doesn't undo a restore made before it in the same isolated child, as in master's
/// `test_successful_restore_survives_reverted_sibling`.
#[test]
fn isolated_child_restore_survives_reverted_sibling() {
    let mut executor = executor_with(&[
        (HANDLER, script_code(&[(NESTED, &[]), INC, (FAILING, &[])])),
        (NESTED, script_code(&[INC, (CHEATS, &[SNAPSHOT]), INC, INC, (CHEATS, &[REVERT_TO, 0])])),
        (FAILING, reverting_script_code(&[])),
    ]);
    executor.isolate = true;

    // 11 after the restore, which undid 13.
    executor.run(&[(HANDLER, &[])]);
    assert_eq!(counter(&executor, 0), 12);
}

/// State captured inside an isolated child keeps the parent's earlier writes in the transaction
/// once it replaces the parent's state. `prepare_isolated_state` makes the child's originals the
/// parent's current values, as the child's gas accounting needs, and `Cache::commit` only accepts
/// entries that differ from their original. Master keeps the writes because its commit takes the
/// present value of every slot of a touched account. Here [`Cheats::child_base`] rebases the
/// captured state onto the parent's originals with [`rebase_isolated_originals`]. Keeping the
/// child's originals loses them: both scripts would end at 10.
#[test]
fn state_captured_in_isolated_child_keeps_parent_writes() {
    // The parent restores a snapshot taken inside a child.
    let contracts = [(HANDLER, script_code(&[(CHEATS, &[SNAPSHOT])]))];
    let script = [INC, (HANDLER, &[]), INC, (CHEATS, &[REVERT_TO, 0])];
    let restored = [false, true].map(|isolate| counter_after(&contracts, &script, isolate));
    assert_eq!(restored, [11, 11], "plain, isolated");

    // A child leaves the fork, which saves the child's state for it.
    let contracts = [(HANDLER, script_code(&[select(1)]))];
    let left =
        [false, true].map(|isolate| counter_after(&contracts, &[INC, (HANDLER, &[])], isolate));
    assert_eq!(left, [11, 11], "plain, isolated");
}

/// A snapshot restore that leaves a fork keeps its overlay and drops its writes since it was
/// selected, also writes the parent made before a child captured the fork's state. The rebase only
/// changes originals, so the overlay keeps holding accepted state alone. Staging the parent's
/// writes in the captured overlay instead would keep them past the restore (11).
#[test]
fn restore_leaving_fork_drops_parent_writes_captured_in_isolated_child() {
    let contracts = [(HANDLER, script_code(&[(CHEATS, &[SNAPSHOT])]))];
    let script = [
        select(1),
        (CHEATS, &[SNAPSHOT]),
        select(0),
        INC,
        (HANDLER, &[]),
        (CHEATS, &[REVERT_TO, 1]),
        (CHEATS, &[REVERT_TO, 0]),
    ];
    let results = [false, true].map(|isolate| counter_after(&contracts, &script, isolate));
    assert_eq!(results, [10, 10], "plain, isolated");
}

/// [`rebase_isolated_originals`] re-adds the parent's entries that the captured state lacks. After
/// a snapshot restore in the child, that brings back writes the restore dropped, so the adapter is
/// skipped while such a restore is in effect: the state then comes from a snapshot whose originals
/// are right already. Without that, the first script would end at 11. A reverted call that undoes
/// the restore turns it back on.
#[test]
fn rebase_skipped_after_restore_in_isolated_child() {
    let contracts = [(HANDLER, script_code(&[(CHEATS, &[REVERT_TO, 0]), (CHEATS, &[SNAPSHOT])]))];
    let script: &[(Address, &[u8])] =
        &[(CHEATS, &[SNAPSHOT]), INC, (HANDLER, &[]), (CHEATS, &[REVERT_TO, 1])];
    let results = [false, true].map(|isolate| counter_after(&contracts, script, isolate));
    assert_eq!(results, [10, 10], "plain, isolated");

    let contracts = [
        (HANDLER, script_code(&[(FAILING, &[]), (CHEATS, &[SNAPSHOT])])),
        (FAILING, reverting_script_code(&[(CHEATS, &[REVERT_TO, 0])])),
    ];
    // Only isolated calls undo restores, so the child's snapshot is taken at 11.
    let script: &[(Address, &[u8])] =
        &[(CHEATS, &[SNAPSHOT]), INC, (HANDLER, &[]), INC, (CHEATS, &[REVERT_TO, 1])];
    assert_eq!(counter_after(&contracts, script, true), 11);
}

/// Rebasing through public API changes metadata, and gas with it. [`rebase_isolated_originals`]
/// gives an account the parent wrote the parent's warmth, where master keeps the child's: cold,
/// with the child's original 11. Restored in the parent, the snapshot leaves `COUNTER` warm, so
/// calling it costs 2,500 gas less than on master (104,688). Restored in the child, `INC` also
/// reads a warm slot (2,000 less), and its write sees the parent's original rather than the child's
/// (2,800 less), which any rebase of the originals, an evm2 one included, would change too: 7,300
/// less than master's 83,688 in all.
#[test]
fn rebased_capture_takes_parent_warmth() {
    let snapshot = || script_code(&[(CHEATS, &[SNAPSHOT])]);
    let (executor, _) = run_isolated(snapshot(), &[INC, (HANDLER, &[])]);
    let snapshot_state = executor.registry.snapshots[0].active_state.clone();
    let mut state = snapshot_state.into_state(EmptyDB::default());
    let account_warm = state.account(&COUNTER).unwrap().is_warm();
    let warm = (account_warm, state.storage(&COUNTER).is_warm(&Word::ZERO));
    let original = state.storage_slot(&COUNTER, Word::ZERO).unwrap().original();
    assert_eq!(
        (warm, original.to::<u64>()),
        ((true, true), 10),
        "account and slot warmth, original"
    );

    let script = [INC, (HANDLER, &[]), (CHEATS, &[REVERT_TO, 0]), INC];
    let (in_parent, in_parent_gas) = run_isolated(snapshot(), &script);
    let restore_in_child = script_code(&[(CHEATS, &[SNAPSHOT]), (CHEATS, &[REVERT_TO, 0]), INC]);
    let (in_child, in_child_gas) = run_isolated(restore_in_child, &[INC, (HANDLER, &[])]);
    assert_eq!([counter(&in_parent, 0), counter(&in_child, 0)], [12, 12]);
    assert_eq!([in_parent_gas, in_child_gas], [104_688 - 2_500, 83_688 - 7_300], "parent, child");
}

/// A failed root frame drops the isolated children's writes, also after a snapshot restore in the
/// root. evm2 rolls the root back through the journal, which `merge_isolated_state` doesn't record,
/// so the root puts back the state it started with, like master's `top_frame_journal`. The caller's
/// nonce, bumped before the root, stays.
#[test]
fn failed_root_drops_isolated_writes() {
    let scripts: [&[(Address, &[u8])]; 2] =
        [&[INC, INC], &[(CHEATS, &[SNAPSHOT]), INC, (CHEATS, &[REVERT_TO, 0]), INC]];
    for script in scripts {
        let counters = [false, true].map(|isolate| {
            let mut executor = executor_with(&[]);
            executor.run(&[INC]);
            executor.isolate = isolate;
            let result = executor.run_code(reverting_script_code(script));
            assert!(!result.status);
            (counter(&executor, 0), nonce(&executor))
        });
        assert_eq!(counters, [(11, 2), (11, 2)], "plain, isolated: {script:?}");
    }
}

/// A failed root frame drops its writes on every fork it switched between, and the fork it ends on
/// gets the persistent accounts as they were when the root started, as a switch then would have.
/// Fork selections stay. evm2 rolls back only the live fork's journal, so this needs the captured
/// state in plain mode too: a fork the root leaves is saved with its writes.
#[test]
fn failed_root_after_fork_switch() {
    let scripts = [
        (&[INC, select(1)] as &[_], 1),
        (&[select(1), INC], 1),
        (&[INC, select(1), INC], 1),
        (&[INC, select(1), INC, select(0)], 0),
    ];
    for (script, active) in scripts {
        let results = [false, true].map(|isolate| {
            let mut executor = executor_with(&[]);
            executor.run(&[INC]);
            executor.isolate = isolate;
            let result = executor.run_code(reverting_script_code(script));
            assert!(!result.status);
            (
                executor.registry.active,
                counter(&executor, 0),
                counter(&executor, 1),
                nonce(&executor),
            )
        });
        assert_eq!(results, [(active, 11, 20, 2); 2], "plain, isolated: {script:?}");
    }
}

/// A failed root also drops its writes on a fork that a snapshot restore switched to.
#[test]
fn failed_root_after_restore_on_another_fork() {
    for isolate in [false, true] {
        let mut executor = executor_with(&[]);
        executor.run(&[select(1), INC]);
        executor.run(&[(CHEATS, &[SNAPSHOT]), select(0)]);
        executor.isolate = isolate;

        let result = executor.run_code(reverting_script_code(&[(CHEATS, &[REVERT_TO, 0]), INC]));
        assert!(!result.status);
        let results = (executor.registry.active, counter(&executor, 0), counter(&executor, 1));
        assert_eq!(results, (1, 10, 21), "isolate: {isolate}");
    }
}

#[test]
fn staged_write_mid_transaction() {
    let backing = BackingDb::with_counter(10);
    let mut executor = Executor::new(vec![backing.clone()]);

    // The nested transaction sees the in-flight write and is accepted at once; the live layer is
    // refreshed, so the next increment continues from it.
    executor.run(&[INC, (CHEATS, &[TRANSACT, 0]), INC]);
    assert_eq!(counter(&executor, 0), 13);

    // A failed read inside the nested transaction publishes nothing.
    executor.run(&[INC]);
    backing.fail_storage(true);
    executor.run(&[(CHEATS, &[TRANSACT, 1])]);
    backing.fail_storage(false);
    assert_eq!(executor.storage(0, COLD_COUNTER, Word::ZERO), Word::from(10));
    assert_eq!(executor.accepted(0, COLD_COUNTER, Word::ZERO), None);
    assert_eq!(counter(&executor, 0), 14);

    // Control: the same nested transaction publishes once reads succeed.
    executor.run(&[(CHEATS, &[TRANSACT, 1])]);
    assert_eq!(executor.accepted(0, COLD_COUNTER, Word::ZERO), Some(Word::from(11)));
}

#[test]
#[ignore = "measurement"]
fn measure_per_call_evm_construction_and_cache_move() {
    const ITERS: u32 = 20_000;
    const SNAPSHOTS: usize = 10;

    fn time(iters: u32, mut f: impl FnMut()) -> Duration {
        let start = Instant::now();
        for _ in 0..iters {
            f();
        }
        start.elapsed() / iters
    }

    let backing = BackingDb::with_counter(10);
    println!(
        "Evm::new (config + tx registry + precompiles): {:?}",
        time(ITERS, || {
            black_box(new_evm(EmptyDB::default()));
        })
    );

    for accounts in [0usize, 1_000, 100_000] {
        let mut state = State::new(EmptyDB::default());
        for i in 0..accounts {
            let address = Address::with_last_byte(0).create(i as u64);
            state.overlay_db_mut().insert_account_info(&address, AccountInfo::default());
            state.overlay_db_mut().insert_account_storage(&address, &Word::ZERO, &Word::from(i));
        }
        let mut saved = Some(SavedState::save(state));
        let moved = time(ITERS, || {
            let mut evm = new_evm(EmptyDB::default());
            *evm.state_mut() = saved.take().unwrap().load(backing.clone());
            let state = mem::replace(evm.state_mut(), State::new(EmptyDB::default()));
            saved = Some(SavedState::save(state));
        });
        let call = time(ITERS, || {
            let mut evm = new_evm(EmptyDB::default());
            *evm.state_mut() = saved.take().unwrap().load(backing.clone());
            let tx = legacy_tx(CALLER, COUNTER, Bytes::new(), 100_000, U256::ZERO);
            assert!(evm.transact(&tx).unwrap().commit().status);
            let state = mem::replace(evm.state_mut(), State::new(EmptyDB::default()));
            saved = Some(SavedState::save(state));
        });
        let loaded = saved.take().unwrap().load(backing.clone());
        let cloned = time(ITERS.min(200), || drop(black_box(loaded.snapshot())));
        // A registry with an inactive fork and snapshots of this size.
        let registry = Registry {
            forks: vec![ForkEntry {
                backing: backing.clone(),
                saved: Some(SavedState::save(loaded.clone())),
            }],
            snapshots: (0..SNAPSHOTS)
                .map(|_| {
                    Arc::new(RegistrySnapshot {
                        active: 0,
                        active_state: loaded.snapshot(),
                        selected: SavedState::default().rest,
                    })
                })
                .collect(),
            ..Default::default()
        };
        let registry_cloned = time(ITERS, || drop(black_box(Registry::clone(&registry))));
        println!(
            "cache of {accounts} accounts + slots: Evm::new + move in/out {moved:?}; \
             with a counter tx {call:?}; StateSnapshot clone {cloned:?}; Registry clone with \
             {SNAPSHOTS} snapshots {registry_cloned:?}"
        );
    }
}

/// `loadAllocs` and `cloneAccount` are cheatcodes, so they run mid-transaction. Written through
/// the transaction layer, as master writes the journal, they are seen by later calls whether the
/// slot was loaded already or not, and accepted with the transaction. A write through
/// `overlay_db_mut` would be shadowed by a loaded slot and overwritten at commit (12).
#[test]
fn state_write_mid_transaction_is_kept() {
    let results = [&[INC, (CHEATS, &[STATE_WRITE]), INC][..], &[(CHEATS, &[STATE_WRITE]), INC]]
        .map(|script| {
            let mut executor = Executor::new(vec![BackingDb::with_counter(10)]);
            executor.run(script);
            counter(&executor, 0)
        });
    assert_eq!(results, [101, 101], "slot loaded before the write, not loaded");

    // Like any write in the transaction, a speculative call sees it and drops it.
    let mut executor = Executor::new(vec![BackingDb::with_counter(10)]);
    executor.set_code(TEST, returning_script_code(&[INC, (CHEATS, &[STATE_WRITE]), INC]));
    let (output, _) = executor.call();
    assert_eq!(Word::from_be_slice(&output), Word::from(101));
    assert_eq!(counter(&executor, 0), 10);
}

/// The active fork's accepted overlay, shared read-only with an [`Executor::call`].
struct AcceptedView {
    cache: Arc<Cache>,
    backing: BackingDb,
}

impl DynDatabase for AcceptedView {
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        match self.cache.accounts.get(address) {
            Some(account) => Ok(account.clone()),
            None => self.backing.get_account(address),
        }
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        match self.cache.contracts.get(code_hash) {
            Some(code) => Ok(code.clone()),
            None => self.backing.get_code_by_hash(code_hash),
        }
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        if let Some(storage) = self.cache.storage.get(address) {
            if let Some(value) = storage.slots.get(key) {
                return Ok(*value);
            }
            if storage.wiped {
                return Ok(Word::ZERO);
            }
        }
        if matches!(self.cache.accounts.get(address), Some(None)) {
            return Ok(Word::ZERO);
        }
        self.backing.get_storage(address, key)
    }

    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        match self.cache.block_hashes.get(number) {
            Some(hash) => Ok(*hash),
            None => self.backing.get_block_hash(number),
        }
    }
}

/// Master's speculative calls take `&self` and borrow the backend (`CowBackend::new_borrowed`).
/// [`Executor::call`] takes `&self` too and shares the state: a call without cheatcode mutations
/// reads the accepted overlay in place and copies neither it nor the registry.
#[test]
fn speculative_call_shares_executor_state() {
    let backing = BackingDb::with_counter(10);
    let mut executor = Executor::new(vec![backing.clone()]);
    executor.run(&[INC]);
    executor.set_code(TEST, returning_script_code(&[INC]));
    let reads = backing.reads();

    let (output, cheats) = executor.call();
    assert_eq!(Word::from_be_slice(&output), Word::from(12));
    assert!(Arc::ptr_eq(&cheats.registry, &executor.registry), "the registry was copied");
    assert!(cheats.shared_overlay.is_some(), "the overlay was copied");
    assert_eq!(backing.reads(), reads, "served from the shared overlay");
    assert_eq!(counter(&executor, 0), 11);
}

/// The first mutation of an [`Executor::call`] comes from a cheatcode mid-transaction. It copies
/// the registry and the accepted overlay before anything saves the live state: a snapshot or a
/// saved fork keeps only the overlay cache, and reloads it over the backing database. The call
/// then switches forks and restores a snapshot on its copies, and the discard leaves the executor
/// as it was. Without the overlay copy, the script ends at 11, as if setUp's write never happened.
#[test]
fn speculative_call_copies_shared_state_at_first_mutation() {
    let script: &[(Address, &[u8])] =
        &[(CHEATS, &[SNAPSHOT]), select(1), INC, select(0), INC, (CHEATS, &[REVERT_TO, 0]), INC];
    for isolate in [false, true] {
        let mut executor = executor_with(&[]);
        executor.isolate = isolate;
        executor.run(&[INC]);
        // Control: the same script, committed by a call that owns the state.
        let mut control = executor.clone();
        control.run(script);
        assert_eq!((counter(&control, 0), counter(&control, 1)), (12, 21), "isolate: {isolate}");

        executor.set_code(TEST, returning_script_code(script));
        let (output, cheats) = executor.call();
        assert_eq!(Word::from_be_slice(&output), Word::from(12), "isolate: {isolate}");
        assert!(!Arc::ptr_eq(&cheats.registry, &executor.registry), "isolate: {isolate}");
        assert_eq!(cheats.registry.snapshots.len(), 1, "isolate: {isolate}");
        drop(cheats);
        assert_eq!(executor.registry.active, 0, "isolate: {isolate}");
        assert!(executor.registry.snapshots.is_empty(), "isolate: {isolate}");
        assert_eq!((counter(&executor, 0), counter(&executor, 1)), (11, 20), "isolate: {isolate}");
    }
}

/// The first mutation can also come from an isolated child, which holds the moved overlay while
/// it runs. The copy goes back to the parent with the overlay.
#[test]
fn speculative_call_copies_shared_state_in_isolated_child() {
    let script: &[(Address, &[u8])] = &[(HANDLER, &[]), INC];
    let mut executor = executor_with(&[(
        HANDLER,
        script_code(&[(CHEATS, &[SNAPSHOT]), select(1), INC, select(0)]),
    )]);
    executor.isolate = true;
    executor.run(&[INC]);
    let mut control = executor.clone();
    control.run(script);
    assert_eq!((counter(&control, 0), counter(&control, 1)), (12, 21));

    executor.set_code(TEST, returning_script_code(script));
    let (output, cheats) = executor.call();
    assert_eq!(Word::from_be_slice(&output), Word::from(12));
    assert!(!Arc::ptr_eq(&cheats.registry, &executor.registry));
    drop(cheats);
    assert_eq!((counter(&executor, 0), counter(&executor, 1)), (11, 20));
}

#[test]
fn registry_and_saved_state_are_send() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SavedState>();
    assert_send_sync::<Registry>();
    assert_send_sync::<Executor>();
}

/// Like `CowBackend`, a call copies only what it changes. A registry-only cheatcode such as
/// `makePersistent` copies the registry, which shares the snapshots, and leaves the accepted
/// overlay shared.
#[test]
fn speculative_call_copies_only_registry_for_registry_op() {
    let script: &[(Address, &[u8])] = &[(HANDLER, &[]), INC];
    for isolate in [false, true] {
        let mut executor = executor_with(&[(HANDLER, script_code(&[PERSIST_COUNTER]))]);
        executor.isolate = isolate;
        executor.run(&[INC, (CHEATS, &[SNAPSHOT])]);

        executor.set_code(TEST, returning_script_code(script));
        let (output, cheats) = executor.call();
        assert_eq!(Word::from_be_slice(&output), Word::from(12), "isolate: {isolate}");
        assert!(cheats.registry.persistent.contains(&COUNTER), "isolate: {isolate}");
        assert!(cheats.shared_overlay.is_some(), "the overlay was copied, isolate: {isolate}");
        assert!(
            Arc::ptr_eq(&cheats.registry.snapshots[0], &executor.registry.snapshots[0]),
            "the snapshot was copied, isolate: {isolate}"
        );
        drop(cheats);
        assert!(!executor.registry.persistent.contains(&COUNTER), "isolate: {isolate}");
        assert_eq!(counter(&executor, 0), 11, "isolate: {isolate}");
    }
}

/// A fork switch after a registry-only cheatcode still copies the overlay before it saves the
/// start fork, and the account made persistent follows the switch. The speculative call leaves the
/// registry and both forks as they were.
#[test]
fn speculative_call_after_registry_op_switches_forks() {
    let script: &[(Address, &[u8])] = &[PERSIST_COUNTER, select(1), INC];
    let mut executor = executor_with(&[]);
    executor.run(&[INC]);
    let mut control = executor.clone();
    control.run(script);
    assert_eq!((counter(&control, 0), counter(&control, 1)), (11, 12));

    executor.set_code(TEST, returning_script_code(script));
    let (output, cheats) = executor.call();
    assert_eq!(Word::from_be_slice(&output), Word::from(12));
    assert!(cheats.shared_overlay.is_none());
    drop(cheats);
    assert!(!executor.registry.persistent.contains(&COUNTER));
    assert_eq!(executor.registry.active, 0);
    assert_eq!((counter(&executor, 0), counter(&executor, 1)), (11, 20));
}

/// Logs belong to the transaction, not to a fork: master keeps one journal across fork switches.
/// Each fork's saved state carries the logs it was saved with, so loading it must not bring them
/// back or drop the ones emitted since, also when an isolated child switches. The next
/// transaction starts without logs.
#[test]
fn logs_survive_fork_switches() {
    let script = [emit(1), select(1), emit(2), select(0), emit(3), select(1)];
    for isolate in [false, true] {
        let mut executor = logging_executor(&[]);
        let logs = logs_after(&mut executor, script_code(&script), isolate);
        assert_eq!(logs, [1, 2, 3], "isolate: {isolate}");
        let logs = logs_after(&mut executor, script_code(&[select(0), emit(4)]), isolate);
        assert_eq!(logs, [4], "isolate: {isolate}");

        // The switch happens inside the handler, which runs in a child with isolation.
        let mut executor =
            logging_executor(&[(HANDLER, script_code(&[emit(2), select(1), emit(3)]))]);
        let script = [emit(1), (HANDLER, &[][..]), emit(4), select(0), emit(5)];
        let logs = logs_after(&mut executor, script_code(&script), isolate);
        assert_eq!(logs, [1, 2, 3, 4, 5], "isolate: {isolate}");
    }
}

/// A snapshot restore keeps the logs emitted since the snapshot, as master's
/// `BackendStateSnapshot::merge` does, whether or not the restore leaves the active fork.
#[test]
fn logs_survive_snapshot_restores() {
    let scripts: [&[(Address, &[u8])]; 2] = [
        &[emit(1), (CHEATS, &[SNAPSHOT]), emit(2), (CHEATS, &[REVERT_TO, 0]), emit(3)],
        &[emit(1), (CHEATS, &[SNAPSHOT]), select(1), emit(2), (CHEATS, &[REVERT_TO, 0]), emit(3)],
    ];
    for (i, script) in scripts.into_iter().enumerate() {
        for isolate in [false, true] {
            let mut executor = logging_executor(&[]);
            let logs = logs_after(&mut executor, script_code(script), isolate);
            assert_eq!(logs, [1, 2, 3], "script {i}, isolate: {isolate}");
        }
    }
}

/// An isolated child's logs come back with its result, as master's log collector sees them. A
/// restore inside the child keeps the child's logs and doesn't bring back the parent's, whichever
/// side took the snapshot.
#[test]
fn logs_survive_restores_in_isolated_children() {
    let cases = [
        (
            &[(CHEATS, &[SNAPSHOT][..]), emit(2), (CHEATS, &[REVERT_TO, 0]), emit(3)][..],
            &[emit(1), (HANDLER, &[][..]), emit(4)][..],
        ),
        (
            &[emit(2), (CHEATS, &[REVERT_TO, 0]), emit(3)],
            &[emit(1), (CHEATS, &[SNAPSHOT]), (HANDLER, &[]), emit(4)],
        ),
    ];
    for (i, (handler, script)) in cases.into_iter().enumerate() {
        for isolate in [false, true] {
            let mut executor = logging_executor(&[(HANDLER, script_code(handler))]);
            let logs = logs_after(&mut executor, script_code(script), isolate);
            assert_eq!(logs, [1, 2, 3, 4], "case {i}, isolate: {isolate}");
        }
    }
}

/// A reverted call drops its own logs and nothing else, also when it restored a snapshot inside an
/// isolated child and the restore is undone. A failed root drops them all.
#[test]
fn reverted_calls_drop_only_their_logs() {
    for isolate in [false, true] {
        let mut executor = logging_executor(&[
            (FAILING, reverting_script_code(&[emit(2)])),
            (HANDLER, script_code(&[emit(3), (FAILING, &[]), emit(4)])),
        ]);
        let script = [emit(1), (FAILING, &[][..]), (HANDLER, &[]), emit(5)];
        let logs = logs_after(&mut executor, script_code(&script), isolate);
        assert_eq!(logs, [1, 3, 4, 5], "isolate: {isolate}");

        let code = reverting_script_code(&[emit(1), select(1), emit(2)]);
        assert!(logs_after(&mut executor, code, isolate).is_empty(), "isolate: {isolate}");
    }

    let mut executor = logging_executor(&[
        (FAILING, reverting_script_code(&[emit(3), (CHEATS, &[REVERT_TO, 0]), emit(4)])),
        (HANDLER, script_code(&[emit(2), (FAILING, &[]), emit(5)])),
    ]);
    let script = [emit(1), (CHEATS, &[SNAPSHOT][..]), (HANDLER, &[]), emit(6)];
    assert_eq!(logs_after(&mut executor, script_code(&script), true), [1, 2, 5, 6]);
}

/// A contract created and self-destructed in one transaction is deleted under EIP-6780, also when
/// its fork is inactive at commit. Committing the fork's transaction layer without finalizing it
/// would keep the account with nonce 1 and slot 0 = 1. [`HANDLER`] creates it and leaves the fork
/// in one call: an isolated child finalizes only the fork it ends on, so in isolated mode the
/// created fork is inactive at commit only when the child itself leaves it.
#[test]
fn create_and_selfdestruct_on_fork_inactive_at_commit() {
    let child = FACTORY.create(0);
    for isolate in [false, true] {
        let mut executor = executor_with(&[
            (FACTORY, factory_code()),
            (HANDLER, script_code(&[(FACTORY, &[]), select(1)])),
        ]);
        executor.isolate = isolate;
        executor.run(&[(HANDLER, &[])]);
        assert_eq!(executor.account(0, child), None, "isolate: {isolate}");
        assert_eq!(executor.storage(0, child, Word::ZERO), Word::ZERO, "isolate: {isolate}");
    }
}
