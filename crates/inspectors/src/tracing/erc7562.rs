//! ERC-7562 tracing inspector
//!
//! The erc7562Tracer reports a call tree in which every frame also lists what its code did: the
//! opcodes it ran, the storage slots it accessed and the code of other accounts it inspected. The
//! root frame lists the preimages of every `KECCAK256` of the transaction.
//!
//! This inspector follows geth's erc7562Tracer. The call tree, with its inputs, outputs and logs,
//! comes from a [`TracingInspector`] that records calls only. Each instruction updates the
//! counters of the frame that runs it as the instruction is about to execute, reading the stack,
//! the memory and the state like geth does. A step that fails counts like any other.
//!
//! A frame that fails at a `KECCAK256` still leaves its preimage, and a preimage can reach 1 MiB of
//! zero padding for a few hundred gas, so the [`TraceLimits`] of the inspector bound the bytes of
//! the preimages it retains, which geth does not. [`DEFAULT_MAX_RECORDED_BYTES`] is the default.
//!
//! See also <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/eth/tracers/native/erc7562.go>

use crate::tracing::{
    DEFAULT_MAX_RECORDED_BYTES, GethTraceBuilder, TraceLimits, TracingInspector,
    TracingInspectorConfig, keccak::memory_copy_padded, limits::TraceBudget,
    utils::load_account_code,
};
use alloc::{borrow::Cow, collections::BTreeSet, format, vec::Vec};
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{
    Address, B256, Bytes, Log, U256,
    map::{Entry, HashMap},
};
use alloy_rpc_types_trace::geth::{
    CallConfig,
    erc7562::{AccessedSlots, ContractSize, Erc7562Config, Erc7562Frame},
};
use evm2::{
    DatabaseError, EvmTypes, Inspector, TxResultExt,
    evm::DbResult,
    interpreter::{InstrStop, Interpreter, Message, MessageResult, opcode::op},
};
use thiserror::Error;

/// The result of an [`Erc7562Inspector`] would be wrong.
#[derive(Clone, Debug, Error)]
pub enum Error {
    /// The inspector reached its byte limit, so the result would miss preimages.
    #[error("erc7562Tracer reached its limit of recorded preimage bytes")]
    LimitExceeded,
    /// The inspector could not read the state that an instruction addressed, so a code size or a
    /// slot value would be wrong.
    #[error("database error {0:?}")]
    Database(DatabaseError),
}

/// Inspector that collects what geth's `erc7562Tracer` reports for a transaction.
#[derive(Clone, Debug)]
pub struct Erc7562Inspector {
    /// Whether the result lists logs.
    with_log: bool,
    /// The opcodes that the used opcode counts leave out, one bit per opcode.
    ignored: U256,
    /// Records the call tree.
    calls: TracingInspector,
    /// What each call did, indexed like the nodes of `calls`.
    frames: Vec<FrameCounters>,
    /// The preimages of the `KECCAK256` operations of the transaction, sorted.
    keccak: BTreeSet<Bytes>,
    /// The bytes of the preimages.
    budget: TraceBudget,
    /// The first error of a read of the state, which the result reports.
    error: Option<DatabaseError>,
    /// The instruction that ran last, in any frame.
    last: Option<LastStep>,
}

/// What a call did.
#[derive(Clone, Debug, Default)]
struct FrameCounters {
    /// The storage slots the call accessed.
    accessed_slots: AccessedSlots,
    /// The accounts whose code an `EXTCODE*` instruction inspected, in order.
    ext_code_access_info: Vec<Address>,
    /// How often the call ran each opcode that is not ignored.
    used_opcodes: HashMap<u8, u64>,
    /// The code size of the accounts that `EXTCODE*` and call instructions addressed.
    contract_size: HashMap<Address, ContractSize>,
}

/// The instruction that ran last.
#[derive(Clone, Copy, Debug)]
struct LastStep {
    /// The opcode.
    op: u8,
    /// The top of the stack before the instruction, if the stack had an item.
    top: Option<U256>,
}

impl Erc7562Inspector {
    /// Returns an inspector that collects the result of the given tracer config.
    pub fn new(config: Erc7562Config) -> Self {
        let with_log = config.with_log.unwrap_or_default();
        let ignored_opcodes = if config.ignored_opcodes.is_empty() {
            (op::PUSH0..=op::SWAP16).chain(DEFAULT_IGNORED_OPCODES).collect()
        } else {
            config.ignored_opcodes
        };
        let mut ignored = U256::ZERO;
        for op in ignored_opcodes {
            ignored.set_bit(usize::from(op), true);
        }
        let calls =
            TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&CallConfig {
                with_log: Some(with_log),
                ..Default::default()
            }));
        let limits =
            TraceLimits::default().set_max_recorded_bytes(Some(DEFAULT_MAX_RECORDED_BYTES));
        Self {
            with_log,
            ignored,
            calls,
            frames: Vec::new(),
            keccak: BTreeSet::new(),
            budget: TraceBudget { limits, ..Default::default() },
            error: None,
            last: None,
        }
    }

    /// Bounds the bytes of the preimages that the inspector retains by the given limits, which
    /// default to [`DEFAULT_MAX_RECORDED_BYTES`].
    pub const fn with_limits(mut self, limits: TraceLimits) -> Self {
        self.budget.limits = limits;
        self
    }

    /// Returns whether a preimage was omitted after reaching the byte limit.
    pub const fn limit_exceeded(&self) -> bool {
        self.budget.exceeded()
    }

    /// Clears what the inspector collected, to trace the next transaction.
    pub fn fuse(&mut self) {
        self.calls.fuse();
        self.frames.clear();
        self.keccak.clear();
        self.budget.recorded = 0;
        self.budget.exceeded = false;
        self.error = None;
        self.last = None;
    }

    /// Sets the index the next recorded log receives, see
    /// [`TracingInspector::set_next_log_index`].
    pub const fn set_next_log_index(&mut self, index: usize) {
        self.calls.set_next_log_index(index);
    }

    /// Returns the result of the tracer for a transaction that ran with this inspector.
    ///
    /// Fails if the byte limit was reached, because the result would miss preimages, or if a read
    /// of the state failed, because a code size or a slot value would be wrong.
    pub fn frame<E>(
        &mut self,
        tx: &Recovered<impl Transaction>,
        result: &TxResultExt<E>,
    ) -> Result<Erc7562Frame, Error> {
        if self.budget.exceeded() {
            return Err(Error::LimitExceeded);
        }
        if let Some(error) = &self.error {
            return Err(Error::Database(error.clone()));
        }
        self.calls.recover_runtime_oog_root(tx, result);
        self.calls.set_transaction_gas_limit(tx.gas_limit());
        self.calls.set_transaction_caller(tx.signer());
        let nodes = self.calls.traces.nodes();

        // Parents precede their children, so a frame knows whether its parent shows logs.
        let mut frames = Vec::<(bool, Erc7562Frame)>::with_capacity(nodes.len());
        for (idx, node) in nodes.iter().enumerate() {
            let counters = self.frames.get(idx).cloned().unwrap_or_default();
            let logs_visible = self.with_log
                && !node.trace.is_error()
                && node.parent.is_none_or(|parent| frames[parent].0);
            let call = node.geth_empty_call_frame(logs_visible);
            let frame = Erc7562Frame {
                call_frame_type: GethTraceBuilder::convert_call_kind(node.kind()),
                from: call.from,
                gas: call.gas.to(),
                gas_used: if idx == 0 { result.tx_gas_used() } else { call.gas_used.to() },
                to: call.to,
                input: call.input,
                output: call.output,
                error: call.error,
                revert_reason: call.revert_reason,
                logs: call.logs,
                value: call.value,
                accessed_slots: counters.accessed_slots,
                ext_code_access_info: counters
                    .ext_code_access_info
                    .iter()
                    .map(|address| format!("{address:?}"))
                    .collect(),
                used_opcodes: counters.used_opcodes,
                contract_size: counters.contract_size,
                // geth reports it for frames that exit with an out of gas error, not for the root.
                out_of_gas: idx != 0 && node.trace.status.is_some_and(InstrStop::is_out_of_gas),
                keccak: if idx == 0 { self.keccak.iter().cloned().collect() } else { Vec::new() },
                calls: Vec::new(),
            };
            frames.push((logs_visible, frame));
        }

        // Children are attached from the back, which reverses their order.
        loop {
            let (_, mut frame) = frames.pop().expect("the root frame exists");
            frame.calls.reverse();
            match nodes[frames.len()].parent {
                Some(parent) => frames[parent].1.calls.push(frame),
                None => return Ok(frame),
            }
        }
    }

    /// Adds the counters of the call a hook started.
    fn push_frame(&mut self) {
        let idx = self.calls.last_trace_idx();
        if self.frames.len() <= idx {
            self.frames.resize_with(idx + 1, FrameCounters::default);
        }
    }

    /// Returns the data that a `KECCAK256` with these operands hashes, if geth records it.
    fn preimage<'a, T: EvmTypes>(
        interp: &'a Interpreter<'_, '_, T>,
        offset: Option<U256>,
        length: Option<U256>,
    ) -> Option<Cow<'a, [u8]>> {
        // The operands are truncated to 64 bits, and geth fails on a negative one.
        let (offset, length) = (offset?.as_limbs()[0], length?.as_limbs()[0]);
        i64::try_from(offset).ok()?;
        i64::try_from(length).ok()?;
        memory_copy_padded(interp.memory().as_slice(), offset, length)
    }

    /// Returns the size of the code of the account, without loading the account.
    fn code_size<T: EvmTypes>(
        interp: &mut Interpreter<'_, '_, T>,
        address: &Address,
    ) -> DbResult<u64> {
        let state = interp.host().state_mut();
        let Some(info) = state.account_info_untracked(address)? else { return Ok(0) };
        let code = load_account_code(state.overlay_db_mut(), &info)?;
        Ok(code.map_or(0, |code| code.len() as u64))
    }
}

/// The opcodes besides `PUSH0` to `SWAP16` that the used opcode counts leave out by default.
const DEFAULT_IGNORED_OPCODES: [u8; 16] = [
    op::POP,
    op::ADD,
    op::SUB,
    op::MUL,
    op::DIV,
    op::EQ,
    op::LT,
    op::GT,
    op::SLT,
    op::SGT,
    op::SHL,
    op::SHR,
    op::AND,
    op::OR,
    op::NOT,
    op::ISZERO,
];

const fn is_ext(op: u8) -> bool {
    matches!(op, op::EXTCODEHASH | op::EXTCODESIZE | op::EXTCODECOPY)
}

const fn is_call(op: u8) -> bool {
    matches!(op, op::CALL | op::CALLCODE | op::DELEGATECALL | op::STATICCALL)
}

/// Keeps the first error of a read of the state, and returns the zero that the read reports.
fn record<V: Default>(first: &mut Option<DatabaseError>, error: DatabaseError) -> V {
    first.get_or_insert(error);
    V::default()
}

fn word_to_address(word: U256) -> Address {
    Address::from_word(word.into())
}

impl<T: EvmTypes> Inspector<T> for Erc7562Inspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.calls.initialize_interp(interp);
    }

    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let op = interp.opcode();
        // The stack can be too short if the instruction is about to fail.
        let (top, second) = match *interp.stack().as_slice() {
            [.., second, top] => (Some(top), Some(second)),
            [top] => (Some(top), None),
            [] => (None, None),
        };
        // A return does not leave its frame's last instruction to the frame that resumes.
        let last = if matches!(op, op::RETURN | op::REVERT) { None } else { self.last };
        let frame = &mut self.frames[self.calls.last_trace_idx()];

        // Only the last `EXTCODE*` instruction per address would matter, but geth lists them all,
        // except an `EXTCODESIZE` that `ISZERO` consumes.
        if let Some(last) = last
            && is_ext(last.op)
            && let Some(top) = last.top
            && !(last.op == op::EXTCODESIZE && op == op::ISZERO)
        {
            frame.ext_code_access_info.push(word_to_address(top));
        }
        if last.is_some_and(|last| last.op == op::GAS) && !is_call(op) {
            *frame.used_opcodes.entry(op::GAS).or_default() += 1;
        }
        if op != op::GAS && !self.ignored.bit(usize::from(op)) {
            *frame.used_opcodes.entry(op).or_default() += 1;
        }

        let slots = &mut frame.accessed_slots;
        match op {
            op if is_ext(op) || is_call(op) => {
                // The code size of the account that the instruction addresses.
                if let Some(word) = if is_ext(op) { top } else { second } {
                    let address = word_to_address(word);
                    if let Entry::Vacant(entry) = frame.contract_size.entry(address) {
                        let contract_size = Self::code_size(interp, &address)
                            .unwrap_or_else(|error| record(&mut self.error, error));
                        entry.insert(ContractSize { contract_size, opcode: op });
                    }
                }
            }
            op::SLOAD => {
                // The value of a slot that the frame did not write or read yet.
                if let Some(slot) = top.map(B256::from)
                    && !slots.reads.contains_key(&slot)
                    && !slots.writes.contains_key(&slot)
                {
                    let address = interp.message().destination;
                    let value = interp
                        .host()
                        .state_mut()
                        .storage_slot_untracked(&address, &slot.into())
                        .unwrap_or_else(|error| record(&mut self.error, error));
                    slots.reads.entry(slot).or_default().push(value.into());
                }
            }
            op::SSTORE => {
                if let Some(slot) = top {
                    *slots.writes.entry(slot.into()).or_default() += 1;
                }
            }
            op::TLOAD => {
                if let Some(slot) = top {
                    *slots.transient_reads.entry(slot.into()).or_default() += 1;
                }
            }
            op::TSTORE => {
                if let Some(slot) = top {
                    *slots.transient_writes.entry(slot.into()).or_default() += 1;
                }
            }
            op::KECCAK256 => {
                if !self.budget.exceeded()
                    && let Some(preimage) = Self::preimage(interp, top, second)
                    && !self.keccak.contains(&*preimage)
                    && self.budget.reserve(preimage.len())
                {
                    self.keccak.insert(Bytes::from(preimage.into_owned()));
                }
            }
            _ => {}
        }
        // Only `EXTCODE*` and `GAS` look at the instruction that ran before them.
        self.last = (is_ext(op) || op == op::GAS).then_some(LastStep { op, top });
        self.budget.halt(interp, true);
    }

    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        Inspector::<T>::log(&mut self.calls, log, host);
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let result = self.calls.call(interp, message);
        self.push_frame();
        result
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.calls.call_end(interp, message, result);
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let result = self.calls.create(interp, message);
        self.push_frame();
        result
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.calls.create_end(interp, message, result);
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        Inspector::<T>::selfdestruct(&mut self.calls, contract, target, value, host);
    }
}
