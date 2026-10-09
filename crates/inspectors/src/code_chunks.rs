//! Observational code-chunk coverage for legacy EVM execution.
//!
//! This measures reads under the existing gas schedule, not EIP-4762 execution. It retains
//! reads from reverted frames (witnesses still need them), excludes initcode and deployment
//! writes, and does not count the EIP-7702 delegation designation read during call resolution.
//! Reset between transactions for transaction-local coverage. No gas or state is modified.

use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, B256, map::HashMap};
use evm2::{
    EvmTypesHost, Inspector,
    bytecode::Bytecode,
    interpreter::{InstrStop, Interpreter, op},
    utils::word_to_usize_saturated,
};

/// Code read by execution or EXTCODECOPY, identified by code address and content hash.
#[derive(Clone, Debug)]
pub struct CodeCoverage {
    /// Address of the code, rather than the storage context for delegate calls.
    pub address: Address,
    /// Hash of the original, unpadded code.
    pub code_hash: B256,
    /// Original code length.
    pub code_len: usize,
    /// Flag per chunk; true if any byte in that chunk was accessed.
    pub touched: Vec<bool>,
}

impl CodeCoverage {
    /// Number of distinct chunks accessed.
    pub fn touched_chunks(&self) -> usize {
        self.touched.iter().filter(|&&touched| touched).count()
    }

    /// Number of original code bytes covered by the touched chunks, excluding padding.
    pub fn covered_bytes(&self, chunk_size: usize) -> usize {
        self.touched
            .iter()
            .enumerate()
            .filter(|(_, touched)| **touched)
            .map(|(index, _)| chunk_size.min(self.code_len - index * chunk_size))
            .sum()
    }
}

#[derive(Clone, Debug, Default)]
struct Frame {
    code: Option<usize>,
    pending: Option<(usize, usize)>,
}

/// Tracks instruction fetches, PUSH operands, jump targets, and code-copy source ranges.
/// Chunk size is configurable for experiments; the database chunk payload is 12 KiB.
#[derive(Clone, Debug)]
pub struct CodeChunkInspector {
    chunk_size: usize,
    codes: Vec<CodeCoverage>,
    indices: HashMap<(Address, B256), usize>,
    frames: Vec<Frame>,
}

impl CodeChunkInspector {
    /// Creates an empty inspector. Panics if `chunk_size` is zero.
    pub fn new(chunk_size: usize) -> Self {
        assert_ne!(chunk_size, 0);
        Self { chunk_size, codes: Vec::new(), indices: HashMap::default(), frames: Vec::new() }
    }

    /// Collected code coverage. Calls sharing a code address and hash share one entry.
    pub fn codes(&self) -> &[CodeCoverage] {
        &self.codes
    }

    /// Clears coverage while retaining the inspector configuration.
    pub fn reset(&mut self) {
        self.codes.clear();
        self.indices.clear();
        self.frames.clear();
    }

    fn register(&mut self, address: Address, code: &Bytecode) -> usize {
        let code_hash = code.hash_slow();
        *self.indices.entry((address, code_hash)).or_insert_with(|| {
            let index = self.codes.len();
            self.codes.push(CodeCoverage {
                address,
                code_hash,
                code_len: code.len(),
                touched: vec![false; code.len().div_ceil(self.chunk_size)],
            });
            index
        })
    }

    fn touch(&mut self, index: usize, start: usize, len: usize) {
        let code = &mut self.codes[index];
        let end = start.saturating_add(len).min(code.code_len);
        if start < end {
            let first = start / self.chunk_size;
            let last = (end - 1) / self.chunk_size;
            code.touched[first..=last].fill(true);
        }
    }
}

impl<T: EvmTypesHost> Inspector<T> for CodeChunkInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let message = interp.message();
        let index = (!message.kind.is_create() && !message.code.is_empty())
            .then(|| self.register(message.code_address, &message.code));
        let depth = usize::from(message.depth);
        self.frames.resize_with(self.frames.len().max(depth + 1), Frame::default);
        self.frames[depth] = Frame { code: index, pending: None };
    }

    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let depth = usize::from(interp.message().depth);
        let frame = &mut self.frames[depth];
        frame.pending = None;
        let Some(index) = frame.code else { return };
        let pc = interp.pc();
        self.touch(index, pc, 1);
        let opcode = interp.opcode();
        let pending = match opcode {
            op::PUSH1..=op::PUSH32 => Some((pc + 1, usize::from(opcode - op::PUSH1 + 1))),
            op::JUMP => {
                interp.stack().peekn::<1>().map(|[target]| (word_to_usize_saturated(target), 1))
            }
            op::JUMPI => interp.stack().peekn::<2>().and_then(|[target, condition]| {
                (!condition.is_zero()).then(|| (word_to_usize_saturated(target), 1))
            }),
            op::CODECOPY => interp.stack().peekn::<3>().map(|[_, offset, len]| {
                (word_to_usize_saturated(offset), word_to_usize_saturated(len))
            }),
            _ => None,
        };
        self.frames[depth].pending = pending;
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let frame = &mut self.frames[usize::from(interp.message().depth)];
        if let Some(index) = frame.code
            && let Some((start, len)) = frame.pending.take()
            && (interp.result().is_ok() || interp.result() == Err(InstrStop::InvalidJump))
        {
            self.touch(index, start, len);
        }
    }

    fn extcodecopy(&mut self, address: Address, code: &Bytecode, offset: usize, len: usize) {
        // Even a zero-length copy currently loads the whole code: retain that denominator.
        if !code.is_empty() {
            let index = self.register(address, code);
            self.touch(index, offset, len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use evm2::{
        BaseEvmTypes, Evm, Precompiles, SpecId,
        env::{BlockEnv, TxEnv},
        evm::{AccountInfo, InMemoryDB},
        interpreter::{Host, Message, MessageResult},
        registry::TxRegistry,
    };

    const ROOT: Address = Address::repeat_byte(0x11);
    const CHILD: Address = Address::repeat_byte(0x22);

    fn run(code: Vec<u8>, db: InMemoryDB, gas: u64) -> (MessageResult, Box<CodeChunkInspector>) {
        let mut message = Message::<BaseEvmTypes> {
            gas_limit: gas,
            destination: ROOT,
            code_address: ROOT,
            call_target: ROOT,
            code: Bytecode::new_legacy(code.into()),
            ..Default::default()
        };
        let new_evm = || {
            Evm::<BaseEvmTypes>::new(
                SpecId::OSAKA,
                BlockEnv::<BaseEvmTypes>::default(),
                TxRegistry::new(),
                db.clone(),
                Precompiles::base(SpecId::OSAKA),
            )
        };
        let mut baseline = new_evm();
        let expected = baseline
            .execute_message(&TxEnv::<BaseEvmTypes>::default(), &mut message.clone())
            .unwrap();
        let mut evm = new_evm();
        evm.set_inspector(CodeChunkInspector::new(31));
        let actual = evm.execute_message(&TxEnv::<BaseEvmTypes>::default(), &mut message).unwrap();
        assert_eq!(actual, expected);
        (actual, evm.clear_inspector_as::<CodeChunkInspector>().unwrap())
    }

    fn touched(trace: &CodeChunkInspector, address: Address) -> Vec<usize> {
        trace
            .codes()
            .iter()
            .filter(|c| c.address == address)
            .flat_map(|c| c.touched.iter().enumerate().filter(|(_, t)| **t).map(|(i, _)| i))
            .collect()
    }

    #[test]
    fn push32_crosses_two_boundaries_and_excludes_padding() {
        let mut code = vec![op::JUMPDEST; 30];
        code.push(op::PUSH32);
        code.extend([0; 32]);
        code.push(op::STOP);
        let (_, trace) = run(code, InMemoryDB::default(), 1000);
        assert_eq!(touched(&trace, ROOT), [0, 1, 2]);
        let (_, trace) = run(vec![op::PUSH32], InMemoryDB::default(), 1000);
        assert_eq!(touched(&trace, ROOT), [0]);
        assert_eq!(trace.codes()[0].covered_bytes(31), 1);
        let (_, trace) = run(vec![], InMemoryDB::default(), 1000);
        assert!(trace.codes().is_empty());
    }

    #[test]
    fn jumps_invalid_targets_untaken_branches_and_oog() {
        let mut code = vec![op::PUSH1, 62, op::JUMP];
        code.resize(94, op::STOP);
        let (result, trace) = run(code.clone(), InMemoryDB::default(), 1000);
        assert_eq!(result.stop, InstrStop::InvalidJump);
        assert_eq!(touched(&trace, ROOT), [0, 2]);
        code[62] = op::JUMPDEST;
        let (_, trace) = run(code.clone(), InMemoryDB::default(), 1000);
        assert_eq!(touched(&trace, ROOT), [0, 2]);
        let (result, trace) = run(code, InMemoryDB::default(), 3);
        assert!(result.stop.is_out_of_gas());
        assert_eq!(touched(&trace, ROOT), [0]);
        let mut code = vec![op::PUSH0, op::PUSH1, 62, op::JUMPI, op::STOP];
        code.resize(94, op::JUMPDEST);
        let (_, trace) = run(code, InMemoryDB::default(), 1000);
        assert_eq!(touched(&trace, ROOT), [0]);
        let (_, trace) = run(vec![op::PUSH1, 255, op::JUMP], InMemoryDB::default(), 1000);
        assert_eq!(touched(&trace, ROOT), [0]);
    }

    #[test]
    fn codecopy_clamps_ranges_and_ignores_empty_reads() {
        for (offset, len, expected) in
            [(60, 5, vec![0, 1, 2]), (93, 255, vec![0, 3]), (62, 0, vec![0]), (255, 2, vec![0])]
        {
            let mut code =
                vec![op::PUSH1, len, op::PUSH1, offset, op::PUSH0, op::CODECOPY, op::STOP];
            code.resize(94, 0);
            let (_, trace) = run(code, InMemoryDB::default(), 10000);
            assert_eq!(touched(&trace, ROOT), expected);
        }
    }

    #[test]
    fn extcodecopy_tracks_unexecuted_code_without_changing_gas() {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &CHILD,
            AccountInfo::default().with_code(Bytecode::new_legacy(vec![op::JUMPDEST; 94].into())),
        );
        for (len, gas, expected) in [(5, 10000, vec![1, 2]), (0, 10000, vec![]), (5, 100, vec![])] {
            let mut code = vec![op::PUSH1, len, op::PUSH1, 60, op::PUSH0, op::PUSH20];
            code.extend(CHILD.as_slice());
            code.extend([op::EXTCODECOPY, op::STOP]);
            let (_, trace) = run(code, db.clone(), gas);
            assert_eq!(touched(&trace, CHILD), expected);
        }
    }

    #[test]
    fn delegatecall_uses_code_address_and_retains_reverted_reads() {
        let mut db = InMemoryDB::default();
        let mut child = vec![op::PUSH1, 62, op::JUMP];
        child.resize(62, 0);
        child.extend([op::JUMPDEST, op::PUSH0, op::PUSH0, op::REVERT]);
        db.insert_account_info(
            &CHILD,
            AccountInfo::default().with_code(Bytecode::new_legacy(child.into())),
        );
        let mut code = vec![op::PUSH0; 4];
        code.push(op::PUSH20);
        code.extend(CHILD.as_slice());
        code.extend([op::PUSH2, 0x27, 0x10, op::DELEGATECALL, op::STOP]);
        let (result, trace) = run(code, db, 100000);
        assert!(result.is_success());
        assert_eq!(touched(&trace, ROOT), [0]);
        assert_eq!(touched(&trace, CHILD), [0, 2]);
        assert_eq!(trace.codes().len(), 2);
    }

    #[test]
    fn repeated_reads_deduplicate_and_reset_is_transaction_local() {
        let mut trace = CodeChunkInspector::new(31);
        let code = Bytecode::new_legacy(vec![op::STOP; 62].into());
        let index = trace.register(ROOT, &code);
        assert_eq!(trace.register(ROOT, &code), index);
        trace.touch(index, 30, 2);
        trace.touch(index, 30, 2);
        assert_eq!(trace.codes()[0].touched_chunks(), 2);
        trace.touch(index, usize::MAX, usize::MAX);
        assert_eq!(trace.codes()[0].touched_chunks(), 2);
        trace.reset();
        assert!(trace.codes().is_empty());
    }

    #[test]
    fn metadata_queries_do_not_touch_external_chunks() {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &CHILD,
            AccountInfo::default().with_code(Bytecode::new_legacy(vec![op::STOP; 94].into())),
        );
        let mut code = Vec::new();
        for opcode in [op::EXTCODESIZE, op::EXTCODEHASH] {
            code.push(op::PUSH20);
            code.extend(CHILD.as_slice());
            code.extend([opcode, op::POP]);
        }
        code.push(op::STOP);
        let (_, trace) = run(code, db, 10000);
        assert_eq!(trace.codes().len(), 1);
        assert!(touched(&trace, CHILD).is_empty());
    }

    #[test]
    fn failed_copy_does_not_touch_source_and_pushdata_is_not_a_jumpdest() {
        let mut code = vec![op::PUSH1, 5, op::PUSH1, 60, op::PUSH0, op::CODECOPY];
        code.resize(94, 0);
        let (result, trace) = run(code, InMemoryDB::default(), 9);
        assert!(result.stop.is_out_of_gas());
        assert_eq!(touched(&trace, ROOT), [0]);
        let mut code = vec![op::PUSH1, 33, op::JUMP];
        code.resize(30, 0);
        code.push(op::PUSH32);
        code.extend([op::JUMPDEST; 32]);
        let (result, trace) = run(code, InMemoryDB::default(), 10000);
        assert_eq!(result.stop, InstrStop::InvalidJump);
        assert_eq!(touched(&trace, ROOT), [0, 1]);
    }
}
