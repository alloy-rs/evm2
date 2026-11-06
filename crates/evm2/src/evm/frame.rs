//! Frame transaction runtime, scoped approvals, and executable-prefix validation.

use super::{
    ExecutionGuard, InterpreterRunner, ScopedInspector, State, frame_diff::FrameDiff,
    inspector::Inspector,
};
use crate::{
    Evm, EvmFeatures, EvmTypes, EvmTypesHost, HostError,
    ethereum::eip8141::FrameValidationResult,
    interpreter::{GasTracker, InstrStop, Message, MessageResult, MessageResultExt},
    registry::HandlerResult,
};
use alloc::{sync::Arc, vec::Vec};
use alloy_consensus::{TxEip8141, transaction::Recovered};
use alloy_eips::{
    Typed2718,
    eip7906::TxTraceParam,
    eip8141::{FrameMode, FrameStatus, NONCE_MANAGER, SignatureScheme, nonce_slot},
};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, U256, map::HashMap};

#[cfg(feature = "async")]
use super::{SendEvmRef, r#async};
#[cfg(feature = "async")]
use crate::registry::HandlerError;
#[cfg(feature = "async")]
use core::future::Future;

/// Payment and execution approvals committed by a call or top-level frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct FrameApprovalState {
    pub(crate) payer: Option<Address>,
    /// Amount actually deducted from the payer at payment approval.
    pub(crate) gas_pre_charge: U256,
    pub(crate) sender_approved: bool,
}

/// Transaction-wide frame progress, scoped approvals, and introspection state.
#[derive(Debug)]
pub(crate) struct FrameTransactionRuntime {
    pub(crate) payload: Arc<TxEip8141>,
    pub(crate) signature_hash: B256,
    pub(crate) max_cost: U256,
    /// Sender account nonce before any transaction frame executes.
    pub(crate) legacy_nonce: u64,
    pub(crate) current_frame_index: usize,
    pub(crate) resolved_target: Address,
    pub(crate) statuses: Vec<FrameStatus>,
    pub(crate) execution_gas_used: Vec<u64>,
    pub(crate) state_gas_used: Vec<u64>,
    pub(crate) approval: FrameApprovalState,
    /// Approval scopes corresponding to active interpreter call frames.
    scopes: Vec<FrameScope>,
    /// Approval produced by the completed top-level interpreter frame.
    root_approval: Option<FrameApprovalState>,
    /// Revertible state-gas attribution across top-level frames.
    state_gas: FrameStateGas,
    /// Assertion snapshot captured on first use in an assertion frame.
    diff: Option<FrameDiff>,
    /// Canonical nonce-key digest, computed once for the immutable transaction payload.
    nonce_keys_hash: Option<B256>,
}

#[derive(Clone, Copy, Debug)]
struct FrameScope {
    approval: FrameApprovalState,
    state_gas_checkpoint: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FrameStateGas {
    /// Top-level frame owning each outstanding storage charge.
    owners: HashMap<(Address, U256), usize>,
    journal: Vec<FrameStateGasJournalEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FrameStateGasJournalEntry {
    Owner { slot: (Address, U256), previous: Option<usize> },
    GasUsed { frame_index: usize, previous: u64 },
}

impl FrameTransactionRuntime {
    pub(crate) fn new(
        payload: Arc<TxEip8141>,
        signature_hash: B256,
        max_cost: U256,
        legacy_nonce: u64,
    ) -> Self {
        let frame_count = payload.frames.len();
        let nonce_keys_hash =
            payload.nonce_keys.as_deref().map(alloy_eips::eip8141::nonce_keys_hash);
        Self {
            resolved_target: payload.sender,
            payload,
            signature_hash,
            max_cost,
            legacy_nonce,
            current_frame_index: 0,
            statuses: Vec::with_capacity(frame_count),
            execution_gas_used: Vec::with_capacity(frame_count),
            state_gas_used: Vec::with_capacity(frame_count),
            approval: FrameApprovalState::default(),
            scopes: Vec::new(),
            root_approval: None,
            state_gas: FrameStateGas::default(),
            diff: None,
            nonce_keys_hash,
        }
    }

    pub(super) fn tx_param(&self, param: U256, state_left: u64) -> Option<U256> {
        let tx = &self.payload;
        Some(match u8::try_from(param).ok()? {
            0 => U256::from(6),
            1 => U256::from(tx.nonce),
            2 => U256::from_be_slice(tx.sender.as_slice()),
            3 => tx.fees.max_priority_fee_per_gas,
            4 => tx.fees.max_fee,
            7 => U256::from(tx.blob_versioned_hashes.len()),
            8 => U256::from_be_bytes(self.signature_hash.0),
            9 => U256::from(tx.frames.len()),
            10 => U256::from(self.current_frame_index),
            11 => U256::from(tx.signatures.len()),
            12 => U256::from(state_left),
            13 if tx.nonce_keys.is_some() => U256::from(self.legacy_nonce),
            14 => U256::from(tx.nonce_keys.as_ref()?.len()),
            15 => U256::from_be_bytes(self.nonce_keys_hash?.0),
            16 => tx.nonce_keys.as_ref()?.first().copied().unwrap_or_default(),
            _ => return None,
        })
    }

    pub(super) fn param(&self, index: U256, param: U256) -> Option<U256> {
        let index = usize::try_from(index).ok()?;
        let f = self.payload.frames.get(index)?;
        Some(match u8::try_from(param).ok()? {
            0 => U256::from_be_slice(f.resolved_target(self.payload.sender).as_slice()),
            1 => U256::from(f.limits.execution),
            2 => U256::from(u8::from(f.mode)),
            3 => U256::from(f.flags),
            4 => U256::from(f.data.len()),
            5 if index < self.current_frame_index => {
                U256::from(u8::from(*self.statuses.get(index)?))
            }
            6 => U256::from(u8::from(f.allowed_scope())),
            7 => U256::from(f.is_atomic_batch() as u8),
            8 => f.value,
            9 => U256::from(f.limits.state),
            10 if index < self.current_frame_index => {
                U256::from(*self.execution_gas_used.get(index)?)
            }
            11 if index < self.current_frame_index => U256::from(*self.state_gas_used.get(index)?),
            _ => return None,
        })
    }

    pub(super) fn signature_param(&self, index: U256, param: U256) -> Option<U256> {
        let sig = self.payload.signatures.get(usize::try_from(index).ok()?)?;
        Some(match u8::try_from(param).ok()? {
            0 if sig.scheme != SignatureScheme::Arbitrary => U256::from_be_slice(
                if sig.signer.is_empty() { self.payload.sender } else { sig.signer_address()? }
                    .as_slice(),
            ),
            1 => U256::from(u8::from(sig.scheme)),
            2 => {
                if sig.signs_transaction_hash() {
                    U256::ZERO
                } else {
                    U256::from_be_bytes(sig.explicit_message()?.0)
                }
            }
            3 if sig.scheme == SignatureScheme::Arbitrary => U256::from(sig.signature.len()),
            _ => return None,
        })
    }

    pub(super) fn is_assert(&self) -> bool {
        self.payload
            .frames
            .get(self.current_frame_index)
            .is_some_and(|f| f.mode == FrameMode::Assert)
    }

    pub(super) fn enter_scope(&mut self) {
        self.scopes.push(FrameScope {
            approval: self.current_approval(),
            state_gas_checkpoint: self.state_gas_checkpoint(),
        });
    }

    /// Exits an interpreter call frame, committing approvals only on success.
    pub(super) fn exit_scope(&mut self, success: bool) {
        let Some(scope) = self.scopes.pop() else {
            return;
        };
        if success {
            self.set_scoped_approval(scope.approval);
        } else {
            self.revert_state_gas(scope.state_gas_checkpoint);
        }
    }

    /// Restores committed approvals after a top-level frame group rolls back.
    pub(crate) fn restore_approval(&mut self, approval: FrameApprovalState) {
        self.approval = approval;
        self.root_approval = None;
        self.scopes.clear();
    }

    pub(crate) const fn commit_root_approval(&mut self) {
        if let Some(state) = self.root_approval.take() {
            self.approval = state;
        }
    }

    pub(crate) const fn state_gas_checkpoint(&self) -> usize {
        self.state_gas.journal.len()
    }

    pub(crate) fn revert_state_gas(&mut self, checkpoint: usize) {
        while self.state_gas.journal.len() > checkpoint {
            match self.state_gas.journal.pop().expect("length checked") {
                FrameStateGasJournalEntry::Owner { slot, previous } => {
                    if let Some(owner) = previous {
                        self.state_gas.owners.insert(slot, owner);
                    } else {
                        self.state_gas.owners.remove(&slot);
                    }
                }
                FrameStateGasJournalEntry::GasUsed { frame_index, previous } => {
                    if let Some(gas_used) = self.state_gas_used.get_mut(frame_index) {
                        *gas_used = previous;
                    }
                }
            }
        }
    }

    /// Attributes a successful storage-creation state-gas charge to the current frame.
    fn record_sstore_state_gas(&mut self, address: Address, key: U256, amount: u64) {
        if amount == 0 {
            return;
        }
        let slot = (address, key);
        let previous_owner = self.state_gas.owners.insert(slot, self.current_frame_index);
        self.state_gas
            .journal
            .push(FrameStateGasJournalEntry::Owner { slot, previous: previous_owner });
        let gas_used = self
            .state_gas_used
            .get_mut(self.current_frame_index)
            .expect("current frame receipt initialized");
        let previous = *gas_used;
        self.state_gas.journal.push(FrameStateGasJournalEntry::GasUsed {
            frame_index: self.current_frame_index,
            previous,
        });
        *gas_used =
            gas_used.checked_add(amount).expect("frame state gas is bounded by its u64 limit");
    }

    /// Applies a storage-restoration refill and returns the amount spendable by this frame.
    fn refill_sstore_state_gas(&mut self, address: Address, key: U256, amount: u64) -> u64 {
        if amount == 0 {
            return 0;
        }
        let slot = (address, key);
        let Some(owner) = self.state_gas.owners.remove(&slot) else {
            return 0;
        };
        self.state_gas
            .journal
            .push(FrameStateGasJournalEntry::Owner { slot, previous: Some(owner) });
        let gas_used =
            self.state_gas_used.get_mut(owner).expect("outstanding charge owner has a receipt");
        let previous = *gas_used;
        self.state_gas
            .journal
            .push(FrameStateGasJournalEntry::GasUsed { frame_index: owner, previous });
        *gas_used = gas_used
            .checked_sub(amount)
            .expect("refill cannot exceed its outstanding state-gas charge");
        if owner == self.current_frame_index { amount } else { 0 }
    }

    pub(super) fn data(&self, index: U256) -> Option<Bytes> {
        self.payload.frames.get(usize::try_from(index).ok()?).map(|frame| frame.data.clone())
    }

    pub(super) fn signature_bytes(&self, index: U256) -> Option<Bytes> {
        let signature = self.payload.signatures.get(usize::try_from(index).ok()?)?;
        (signature.scheme == SignatureScheme::Arbitrary).then(|| signature.signature.clone())
    }

    pub(super) fn sstore_gas(
        &mut self,
        address: Address,
        key: U256,
        charge: u64,
        refill: u64,
    ) -> u64 {
        self.record_sstore_state_gas(address, key, charge);
        self.refill_sstore_state_gas(address, key, refill)
    }

    pub(super) fn trace_value(
        &mut self,
        state: &State<'_>,
        param: U256,
        index: U256,
    ) -> Option<U256> {
        if !self.is_assert() {
            return None;
        }
        let selector = TxTraceParam::try_from(u8::try_from(param).ok()?).ok()?;
        if selector.requires_zero_index() && !index.is_zero() {
            return None;
        }
        let index = usize::try_from(index).ok()?;
        let logs = state.logs();
        Some(match selector {
            TxTraceParam::EventsCount => U256::from(logs.len()),
            TxTraceParam::EventAddress => U256::from_be_slice(logs.get(index)?.address.as_slice()),
            TxTraceParam::EventTopicCount => U256::from(logs.get(index)?.data.topics().len()),
            TxTraceParam::EventTopic0
            | TxTraceParam::EventTopic1
            | TxTraceParam::EventTopic2
            | TxTraceParam::EventTopic3 => U256::from_be_bytes(
                logs.get(index)?.data.topics().get(usize::from(u8::from(selector) - 0x14))?.0,
            ),
            TxTraceParam::EventDataLength => U256::from(logs.get(index)?.data.data.len()),
            TxTraceParam::GasPreCharge => self.approval.gas_pre_charge,
            TxTraceParam::GasPayerAddress => U256::from_be_slice(self.approval.payer?.as_slice()),
            selector => {
                return self
                    .diff
                    .get_or_insert_with(|| FrameDiff::capture(state))
                    .value(selector, index);
            }
        })
    }

    pub(super) fn event_data(&self, state: &State<'_>, index: U256) -> Option<Bytes> {
        if !self.is_assert() {
            return None;
        }
        state.logs().get(usize::try_from(index).ok()?).map(|log| log.data.data.clone())
    }

    fn current_approval(&self) -> FrameApprovalState {
        self.scopes.last().map_or(self.approval, |scope| scope.approval)
    }

    fn set_scoped_approval(&mut self, approval: FrameApprovalState) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.approval = approval;
        } else {
            self.root_approval = Some(approval);
        }
    }
}

impl<T: EvmTypes> Evm<'_, T> {
    pub(super) fn apply_frame_approval(
        &mut self,
        address: Address,
        scope: U256,
        state_left: u64,
    ) -> Result<u64, HostError> {
        let rt = self.frame_tx.as_ref().ok_or(InstrStop::OpcodeNotFound)?;
        if rt.is_assert() {
            return Err(InstrStop::StateChangeDuringStaticCall.into());
        }
        let f = &rt.payload.frames[rt.current_frame_index];
        let scope = u8::try_from(scope).map_err(|_| InstrStop::Revert)?;
        if address != rt.resolved_target || scope == 0 || scope & !(f.flags & 3) != 0 {
            return Err(InstrStop::Revert.into());
        }
        let old = rt.current_approval();
        let sender = rt.payload.sender;
        let execution = scope & 2 != 0;
        let payment = scope & 1 != 0;
        if execution && (old.sender_approved || address != sender) {
            return Err(InstrStop::Revert.into());
        }
        if payment && (old.payer.is_some() || (!old.sender_approved && !execution)) {
            return Err(InstrStop::Revert.into());
        }
        let keys = rt.payload.nonce_keys.as_deref().filter(|keys| !keys.is_empty());
        let sequence = rt.payload.nonce;
        let maximum = rt.max_cost;
        let mut charged = 0u64;
        let mut precharge = old.gas_pre_charge;
        if payment {
            if let Some(keys) = keys {
                for key in keys {
                    let first = self
                        .state
                        .storage(&NONCE_MANAGER)?
                        .into_slot(nonce_slot(sender, *key))?
                        .current()
                        .is_zero();
                    if first {
                        charged = charged
                            .checked_add(
                                self.version()
                                    .gas_params
                                    .get(crate::version::GasId::SstoreSetState),
                            )
                            .ok_or(InstrStop::OutOfGas)?;
                    }
                }
            } else {
                let empty = {
                    let account = self.state.account(&sender)?;
                    if account.nonce() == u64::MAX {
                        return Err(InstrStop::OutOfGas.into());
                    }
                    account.get().is_none_or(super::AccountInfo::is_empty)
                };
                if empty {
                    charged = self.version().gas_params.new_account_state_gas();
                }
            }
            let balance = self.state.account(&address)?.balance();
            if self.feature(EvmFeatures::BALANCE_CHECK) && balance < maximum {
                return Err(InstrStop::Revert.into());
            }
            if charged > state_left {
                return Err(InstrStop::OutOfGas.into());
            }
            if let Some(keys) = keys {
                // MAX is the single-use marker: consuming it exhausts the selected keys.
                let next = sequence.saturating_add(1);
                for key in keys {
                    self.state
                        .storage(&NONCE_MANAGER)?
                        .into_slot(nonce_slot(sender, *key))?
                        .set(U256::from(next));
                }
            } else {
                self.state.account(&sender)?.bump_nonce();
            }
            let fees = self.feature(EvmFeatures::FEE_CHARGE);
            if fees {
                self.state.account(&address)?.set_balance(balance.saturating_sub(maximum));
            } else if !self.feature(EvmFeatures::BALANCE_CHECK)
                && self.feature(EvmFeatures::BALANCE_TOP_UP)
                && balance < maximum
            {
                self.state.account(&address)?.set_balance(maximum);
            }
            precharge = if fees { balance.min(maximum) } else { U256::ZERO };
        }
        let rt = self.frame_tx.as_mut().unwrap();
        let next = FrameApprovalState {
            payer: if payment { Some(address) } else { old.payer },
            gas_pre_charge: precharge,
            sender_approved: old.sender_approved || execution,
        };
        rt.set_scoped_approval(next);
        Ok(charged)
    }

    pub(super) fn execute_default_frame(
        &mut self,
        message: &Message<T>,
    ) -> Result<Option<MessageResult<T>>, crate::ExecutionError> {
        let Some(rt) = &self.frame_tx else {
            return Ok(None);
        };
        if message.depth != self.version().initial_call_depth
            || rt.payload.frames[rt.current_frame_index].mode != FrameMode::Verify
            || self.precompiles().contains(&message.call_target)
            || self.state.account(&message.destination)?.code_hash() != KECCAK256_EMPTY
        {
            return Ok(None);
        }
        let f = &rt.payload.frames[rt.current_frame_index];
        let scope = f.flags & 3;
        let index = if scope & 2 != 0 { 0 } else { 1 };
        let valid = scope != 0
            && rt.payload.signatures.get(index).is_some_and(|sig| {
                sig.scheme == SignatureScheme::Secp256k1
                    && sig.signs_transaction_hash()
                    && if sig.signer.is_empty() {
                        rt.payload.sender == message.destination
                    } else {
                        sig.signer_address() == Some(message.destination)
                    }
            });
        let mut gas = GasTracker::new_isolated(message.gas_limit, message.reservoir);
        let stop = if valid {
            match self.apply_frame_approval(message.destination, U256::from(scope), gas.reservoir())
            {
                Ok(cost) => {
                    gas.spend_state(cost).expect("approval checked state budget");
                    InstrStop::Return
                }
                Err(HostError::Halt(stop)) => stop,
                Err(HostError::Execution(error)) => return Err(error),
            }
        } else {
            InstrStop::Revert
        };
        gas.settle_gas(stop);
        Ok(Some(MessageResultExt { stop, gas, output: Bytes::new(), ..Default::default() }))
    }

    /// Invokes a frame lifecycle hook while preventing inspector mutation.
    pub(crate) fn inspect_frame(&mut self, f: impl FnOnce(&mut dyn Inspector<T>, &mut Self)) {
        let guard = self.enter_execution();
        if let Some(inspector) = guard.evm.active_inspector() {
            // SAFETY: the execution guard prevents replacing the active inspector.
            let inspector = unsafe { &mut *(inspector as *mut dyn Inspector<T>) };
            f(inspector, guard.evm);
        }
    }

    fn simulate_frames<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        assert!(!self.running, "prefix validation cannot interrupt active execution");
        let runner = self.interpreter_runner.take();
        let mut execution = self.enter_execution();
        execution.discard_on_drop = true;
        let guard = FrameSimulationGuard { execution, runner };
        f(guard.execution.evm)
    }
}

struct FrameSimulationGuard<'guard, 'evm, T: EvmTypesHost> {
    execution: ExecutionGuard<'guard, 'evm, T>,
    runner: Option<Arc<dyn InterpreterRunner<T>>>,
}

impl<T: EvmTypesHost> Drop for FrameSimulationGuard<'_, '_, T> {
    fn drop(&mut self) {
        self.execution.evm.interpreter_runner = self.runner.take();
    }
}

impl<'a, T: EvmTypes<Tx: Typed2718>> Evm<'a, T> {
    /// Validates all signatures, executes only the payable prefix, and discards all effects.
    ///
    /// Prefix execution always uses the interpreter, including when full transactions use a
    /// compiled runner. This preserves per-opcode admission observations in reverted calls.
    pub fn validate_frame_prefix(
        &mut self,
        tx: &Recovered<T::Tx>,
        prefix_end: usize,
    ) -> HandlerResult<FrameValidationResult> {
        let handler = self.registry.try_get_by_type(tx.ty())?;
        self.simulate_frames(|host| {
            handler.validate_frame_prefix(tx.as_recovered_ref(), host, prefix_end)
        })
    }

    /// Executes a discarded frame prefix using a borrowed inspector.
    ///
    /// Restores the previous inspector on success, error, or unwind.
    pub fn validate_frame_prefix_with_inspector(
        &mut self,
        tx: &Recovered<T::Tx>,
        prefix_end: usize,
        inspector: &mut (dyn Inspector<T> + '_),
    ) -> HandlerResult<FrameValidationResult> {
        self.assert_inspector_mutable();
        let mut scope = ScopedInspector::install(self, inspector);
        let result = scope.evm.validate_frame_prefix(tx, prefix_end);
        scope.completed = true;
        result
    }

    /// Executes and discards a frame validation prefix on an async host fiber.
    ///
    /// Dropping the future cancels execution and discards all prefix effects.
    #[cfg(feature = "async")]
    pub fn validate_frame_prefix_async<'fut>(
        &'fut mut self,
        tx: &'fut Recovered<T::Tx>,
        prefix_end: usize,
    ) -> impl Future<Output = r#async::AsyncResult<FrameValidationResult, HandlerError>> + 'fut
    {
        let stack = self.async_stack();
        // SAFETY: The future exclusively borrows the EVM and its stack slot until dropped.
        unsafe {
            r#async::on_local_fiber_result_with_stack(stack, move || {
                self.validate_frame_prefix(tx, prefix_end)
            })
        }
    }

    /// Executes a frame validation prefix on an async host fiber with a `Send` future.
    ///
    /// As with [`Self::transact_async_send`], first verify the current erased database,
    /// precompiles, and optional inspector using [`Self::evm_is_send`] or
    /// [`Self::evm_is_send_with_inspector`]. Cancellation discards all prefix effects.
    #[cfg(feature = "async")]
    pub fn validate_frame_prefix_async_send<'fut>(
        &'fut mut self,
        tx: &'fut Recovered<T::Tx>,
        prefix_end: usize,
    ) -> impl Future<Output = r#async::AsyncResult<FrameValidationResult, HandlerError>> + Send + 'fut
    where
        T::SpecId: Send,
        T::Tx: Send + Sync,
        T::EvmExt: Send,
        T::MessageExt: Send,
        T::MessageResultExt: Send,
        T::TxEnvExt: Send,
        T::TxResultExt: Send,
        T::BlockEnvExt: Send,
    {
        self.assert_erased_send();
        let stack = self.async_stack();
        let evm = SendEvmRef { evm: self };
        // SAFETY: The future owns the exclusive EVM borrow and its erased fields were
        // checked above. The reusable stack remains borrowed until the future is dropped.
        unsafe {
            r#async::on_fiber_result_with_stack(stack, move || {
                let SendEvmRef { evm } = evm;
                evm.validate_frame_prefix(tx, prefix_end)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::CacheDB;
    use alloy_eips::eip8141::Frame;
    use alloy_primitives::{Log, LogData, b256, keccak256};

    #[test]
    fn txparam_exposes_signed_fee_budget_and_rejects_removed_fee_selectors() {
        let mut tx = TxEip8141::default();
        tx.fees.max_priority_fee_per_gas = U256::from(7);
        tx.fees.max_fee = U256::MAX;
        let runtime = FrameTransactionRuntime::new(Arc::new(tx), B256::ZERO, U256::from(19), 0);
        assert_eq!(runtime.tx_param(U256::from(3), 0), Some(U256::from(7)));
        assert_eq!(runtime.tx_param(U256::from(4), 0), Some(U256::MAX));
        for removed in [5, 6] {
            assert_eq!(runtime.tx_param(U256::from(removed), 0), None);
        }
        assert_eq!(runtime.tx_param(U256::from(7), 0), Some(U256::ZERO));
    }

    #[test]
    fn txparam_caches_canonical_nonce_key_hashes_without_collapsing_absent_and_empty_keys() {
        let mut zero_key_preimage = [0; 64];
        zero_key_preimage[31] = 1;
        let keys = (0..16).map(U256::from).collect::<Vec<_>>();
        let mut max_keys_preimage = [0; 17 * 32];
        max_keys_preimage[31] = 16;
        for (index, key) in keys.iter().enumerate() {
            max_keys_preimage[(index + 1) * 32..(index + 2) * 32]
                .copy_from_slice(&key.to_be_bytes::<32>());
        }
        for (keys, expected) in [
            (None, None),
            (
                Some(alloc::vec![]),
                Some(b256!("290decd9548b62a8d60345a988386fc84ba6bc95484008f6362f93160ef3e563")),
            ),
            (Some(alloc::vec![U256::ZERO]), Some(keccak256(zero_key_preimage))),
            (
                Some(alloc::vec![U256::ONE]),
                Some(b256!("cc69885fda6bcc1a4ace058b4a62bf5e179ea78fd58a1ccd71c22cc9b688792f")),
            ),
            (Some(keys), Some(keccak256(max_keys_preimage))),
        ] {
            let runtime = FrameTransactionRuntime::new(
                Arc::new(TxEip8141 { nonce_keys: keys.clone(), ..Default::default() }),
                B256::ZERO,
                U256::ZERO,
                7,
            );
            assert_eq!(runtime.nonce_keys_hash, expected);
            for _ in 0..32 {
                assert_eq!(
                    runtime.tx_param(U256::from(15), 0),
                    expected.map(|hash| U256::from_be_bytes(hash.0))
                );
                assert_eq!(
                    runtime.tx_param(U256::from(14), 0),
                    keys.as_ref().map(|keys| U256::from(keys.len()))
                );
                assert_eq!(
                    runtime.tx_param(U256::from(16), 0),
                    keys.as_ref().map(|keys| keys.first().copied().unwrap_or_default())
                );
            }
        }
    }

    #[test]
    fn cheap_and_invalid_txtrace_queries_leave_the_state_diff_unmaterialized() {
        let changed = Address::with_last_byte(5);
        let payer = Address::with_last_byte(9);
        let mut state = State::new(CacheDB::default());
        state.account(&changed).unwrap().set_balance(U256::from(11));
        state.storage(&changed).unwrap().into_slot(U256::ONE).unwrap().set(U256::from(12));
        state.log(Log {
            address: changed,
            data: LogData::new_unchecked(
                (1..=4).map(B256::with_last_byte).collect(),
                Bytes::from_static(&[7, 8, 9]),
            ),
        });
        state.log(Log { address: changed, data: LogData::default() });
        let mut runtime = FrameTransactionRuntime::new(
            Arc::new(TxEip8141 {
                frames: alloc::vec![Frame { mode: FrameMode::Assert, ..Default::default() }],
                ..Default::default()
            }),
            B256::ZERO,
            U256::ZERO,
            0,
        );
        runtime.approval = FrameApprovalState {
            payer: Some(payer),
            gas_pre_charge: U256::from(17),
            sender_approved: true,
        };
        for (selector, expected) in [
            (TxTraceParam::EventsCount, 2),
            (TxTraceParam::EventAddress, 5),
            (TxTraceParam::EventTopicCount, 4),
            (TxTraceParam::EventTopic0, 1),
            (TxTraceParam::EventTopic1, 2),
            (TxTraceParam::EventTopic2, 3),
            (TxTraceParam::EventTopic3, 4),
            (TxTraceParam::EventDataLength, 3),
            (TxTraceParam::GasPreCharge, 17),
            (TxTraceParam::GasPayerAddress, 9),
        ] {
            assert_eq!(
                runtime.trace_value(&state, U256::from(u8::from(selector)), U256::ZERO),
                Some(U256::from(expected))
            );
            assert!(runtime.diff.is_none());
        }
        for (selector, index) in [
            (U256::MAX, U256::ZERO),
            (U256::from(0xff), U256::ZERO),
            (U256::ZERO, U256::ONE),
            (U256::from(0x11), U256::ONE),
            (U256::from(0x19), U256::ONE),
            (U256::from(0x1a), U256::ONE),
            (U256::from(4), U256::MAX),
            (U256::from(0x12), U256::from(2)),
            (U256::from(0x14), U256::ONE),
        ] {
            assert_eq!(runtime.trace_value(&state, selector, index), None);
            assert!(runtime.diff.is_none());
        }
        runtime.approval.payer = None;
        assert_eq!(runtime.trace_value(&state, U256::from(0x1a), U256::ZERO), None);
        assert!(runtime.diff.is_none());
        assert_eq!(runtime.trace_value(&state, U256::ONE, U256::ZERO), Some(U256::ONE));
        assert!(runtime.diff.is_some());
        assert_eq!(runtime.trace_value(&state, U256::from(0x0a), U256::ZERO), Some(U256::ONE));
        assert_eq!(runtime.trace_value(&state, U256::from(0x09), U256::ZERO), Some(U256::from(12)));
    }
}
