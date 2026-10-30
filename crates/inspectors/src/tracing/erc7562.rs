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
//! See also <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/eth/tracers/native/erc7562.go>

use crate::tracing::{
    GethTraceBuilder, TracingInspector, TracingInspectorConfig, keccak::memory_copy_padded,
    utils::load_account_code,
};
use alloc::{collections::BTreeSet, format, vec::Vec};
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, Log, U256, map::HashMap};
use alloy_rpc_types_trace::geth::{
    CallConfig,
    erc7562::{AccessedSlots, ContractSize, Erc7562Config, Erc7562Frame},
};
use evm2::{
    EvmTypes, Inspector, TxResultExt,
    interpreter::{InstrStop, Interpreter, Message, MessageResult, opcode::op},
};

/// Inspector that collects what geth's `erc7562Tracer` reports for a transaction.
#[derive(Clone, Debug)]
pub struct Erc7562Inspector {
    /// The tracer config.
    config: Erc7562Config,
    /// The opcodes that the used opcode counts leave out, one bit per opcode.
    ignored: U256,
    /// Records the call tree.
    calls: TracingInspector,
    /// What each call did, indexed like the nodes of `calls`.
    frames: Vec<FrameCounters>,
    /// The preimages of the `KECCAK256` operations of the transaction, sorted.
    keccak: BTreeSet<Bytes>,
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
        let mut ignored = U256::ZERO;
        let defaults = (op::PUSH0..=op::SWAP16).chain(DEFAULT_IGNORED_OPCODES);
        for op in if config.ignored_opcodes.is_empty() {
            defaults.collect()
        } else {
            config.ignored_opcodes.clone()
        } {
            ignored.set_bit(usize::from(op), true);
        }
        let calls =
            TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&CallConfig {
                with_log: config.with_log,
                ..Default::default()
            }));
        Self { config, ignored, calls, frames: Vec::new(), keccak: BTreeSet::new(), last: None }
    }

    /// Returns the tracer config.
    pub const fn config(&self) -> &Erc7562Config {
        &self.config
    }

    /// Clears what the inspector collected, to trace the next transaction.
    pub fn fuse(&mut self) {
        self.calls.fuse();
        self.frames.clear();
        self.keccak.clear();
        self.last = None;
    }

    /// Sets the index the next recorded log receives, see
    /// [`TracingInspector::set_next_log_index`].
    pub const fn set_next_log_index(&mut self, index: usize) {
        self.calls.set_next_log_index(index);
    }

    /// Returns the result of the tracer for a transaction that ran with this inspector.
    pub fn frame<E>(
        &mut self,
        tx: &Recovered<impl Transaction>,
        result: &TxResultExt<E>,
    ) -> Erc7562Frame {
        self.calls.recover_runtime_oog_root(tx, result);
        let nodes = self.calls.traces.nodes();
        let include_logs = self.config.with_log.unwrap_or_default();

        // Parents precede their children, so a frame knows whether its parent shows logs.
        let mut frames = Vec::<(bool, Erc7562Frame)>::with_capacity(nodes.len());
        for (idx, node) in nodes.iter().enumerate() {
            let counters = self.frames.get(idx).cloned().unwrap_or_default();
            let logs_visible = include_logs
                && !node.trace.is_error()
                && node.parent.is_none_or(|parent| frames[parent].0);
            let call = node.geth_empty_call_frame(logs_visible);
            let frame = Erc7562Frame {
                call_frame_type: GethTraceBuilder::convert_call_kind(node.kind()),
                from: if idx == 0 { tx.signer() } else { call.from },
                gas: if idx == 0 { tx.gas_limit() } else { call.gas.to() },
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
                None => return frame,
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

    /// Returns the data a `KECCAK256` with the operands hashes, if geth records it.
    fn preimage<T: EvmTypes>(
        interp: &Interpreter<'_, '_, T>,
        offset: Option<U256>,
        length: Option<U256>,
    ) -> Option<Bytes> {
        // The operands are truncated to 64 bits, and geth fails on a negative one.
        let (offset, length) = (offset?.as_limbs()[0], length?.as_limbs()[0]);
        i64::try_from(offset).ok()?;
        i64::try_from(length).ok()?;
        memory_copy_padded(interp.memory().as_slice(), offset, length)
            .map(|preimage| Bytes::copy_from_slice(&preimage))
    }

    /// Returns the size of the code of the account, without loading the account.
    fn code_size<T: EvmTypes>(interp: &mut Interpreter<'_, '_, T>, address: &Address) -> u64 {
        let state = interp.host().state_mut();
        let code = state
            .account_info_untracked(address)
            .ok()
            .flatten()
            .and_then(|info| load_account_code(state.initial_mut(), &info).ok().flatten());
        code.map_or(0, |code| code.len() as u64)
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

fn address(word: U256) -> Address {
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
        let idx = self.calls.last_trace_idx();

        // What the instruction reads from the interpreter, before the frame is borrowed.
        let (mut sized, mut slot, mut preimage) = (None, None, None);
        match op {
            op::EXTCODESIZE | op::EXTCODECOPY | op::EXTCODEHASH => sized = top.map(address),
            op::CALL | op::CALLCODE | op::DELEGATECALL | op::STATICCALL => {
                sized = second.map(address);
            }
            op::SLOAD => slot = top.map(B256::from),
            op::KECCAK256 => preimage = Self::preimage(interp, top, second),
            _ => {}
        }
        let size = sized
            .filter(|address| !self.frames[idx].contract_size.contains_key(address))
            .map(|address| (address, Self::code_size(interp, &address)));
        // The value of a slot that an `SLOAD` reads before the frame wrote or read it.
        let read = slot
            .filter(|slot| {
                let slots = &self.frames[idx].accessed_slots;
                !slots.reads.contains_key(slot) && !slots.writes.contains_key(slot)
            })
            .map(|slot| {
                let address = interp.message().destination;
                let value =
                    interp.host().state_mut().storage_slot_untracked(&address, &slot.into());
                (slot, B256::from(value.unwrap_or_default()))
            });

        let frame = &mut self.frames[idx];
        // Only the last `EXTCODE*` instruction per address would matter, but geth lists them all,
        // except an `EXTCODESIZE` that `ISZERO` consumes.
        if let Some(last) = last
            && is_ext(last.op)
            && let Some(top) = last.top
            && !(last.op == op::EXTCODESIZE && op == op::ISZERO)
        {
            frame.ext_code_access_info.push(address(top));
        }
        if let Some((address, size)) = size {
            frame.contract_size.insert(address, ContractSize { contract_size: size, opcode: op });
        }
        if last.is_some_and(|last| last.op == op::GAS) && !is_call(op) {
            *frame.used_opcodes.entry(op::GAS).or_default() += 1;
        }
        if op != op::GAS && !self.ignored.bit(usize::from(op)) {
            *frame.used_opcodes.entry(op).or_default() += 1;
        }
        let slots = &mut frame.accessed_slots;
        if let Some((slot, value)) = read {
            slots.reads.entry(slot).or_default().push(value);
        }
        match (op, top) {
            (op::SSTORE, Some(slot)) => *slots.writes.entry(slot.into()).or_default() += 1,
            (op::TLOAD, Some(slot)) => *slots.transient_reads.entry(slot.into()).or_default() += 1,
            (op::TSTORE, Some(slot)) => {
                *slots.transient_writes.entry(slot.into()).or_default() += 1;
            }
            _ => {}
        }
        if let Some(preimage) = preimage {
            self.keccak.insert(preimage);
        }
        self.last = Some(LastStep { op, top });
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
