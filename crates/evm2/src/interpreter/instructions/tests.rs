use crate::{
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{Gas, Host, InstrStop, Interpreter, Message, SpecId, Word, op},
};
use alloc::vec::Vec;
use alloy_primitives::{B256, Bytes, Log};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(in crate::interpreter) struct TestHost {
    pub(super) block: BlockEnv,
    pub(super) code_size: usize,
    pub(super) code_hash: B256,
    pub(super) storage: HashMap<Word, Word>,
    pub(super) transient_storage: HashMap<Word, Word>,
    pub(super) logs: Vec<Log>,
}

impl Host for TestHost {
    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn balance(&mut self, address: Word) -> Word {
        address
    }

    fn get_code_size(&mut self, _address: Word) -> usize {
        self.code_size
    }

    fn get_code_hash(&mut self, _address: Word) -> B256 {
        self.code_hash
    }

    fn block_hash(&mut self, number: u64) -> Option<B256> {
        Some(B256::with_last_byte(number as u8))
    }

    fn sload(&mut self, index: Word) -> Word {
        self.storage.get(&index).copied().unwrap_or_default()
    }

    fn sstore(&mut self, index: Word, value: Word) {
        self.storage.insert(index, value);
    }

    fn tload(&mut self, index: Word) -> Word {
        self.transient_storage.get(&index).copied().unwrap_or_default()
    }

    fn tstore(&mut self, index: Word, value: Word) {
        self.transient_storage.insert(index, value);
    }

    fn log(&mut self, log: Log) {
        self.logs.push(log);
    }
}

pub(super) struct TestInterpreter {
    pub(super) inner: Interpreter,
    pub(super) err: InstrStop,
}

impl TestInterpreter {
    pub(super) fn stack(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.inner.stack.as_ptr(), self.inner.stack_len) }
    }

    pub(super) fn memory(&mut self, offset: usize, len: usize) -> &[u8] {
        self.inner.memory.slice(offset, len).unwrap()
    }

    pub(super) fn gas_remaining(&self) -> u64 {
        self.inner.gas.remaining()
    }
}

pub(super) fn run(code: impl Into<Vec<u8>>) -> TestInterpreter {
    let mut host = TestHost::default();
    run_with_host(code, &mut host)
}

pub(super) fn run_with_host(code: impl Into<Vec<u8>>, host: &mut dyn Host) -> TestInterpreter {
    run_with_host_and_spec(code, host, SpecId::HOMESTEAD)
}

pub(super) fn run_with_host_and_spec(
    code: impl Into<Vec<u8>>,
    host: &mut dyn Host,
    spec_id: SpecId,
) -> TestInterpreter {
    run_with_host_and_spec_config(code, host, spec_id, false, 10_000)
}

pub(super) fn run_with_host_tx_env(
    code: impl Into<Vec<u8>>,
    host: &mut dyn Host,
    tx_env: TxEnv,
) -> TestInterpreter {
    run_with_host_tx_env_and_spec(code, host, tx_env, SpecId::HOMESTEAD)
}

pub(super) fn run_with_host_tx_env_and_spec(
    code: impl Into<Vec<u8>>,
    host: &mut dyn Host,
    tx_env: TxEnv,
    spec_id: SpecId,
) -> TestInterpreter {
    run_with_host_message_tx_env_and_spec_config(
        code,
        host,
        Message { gas_limit: 10_000, ..Message::default() },
        tx_env,
        spec_id,
        false,
    )
}

pub(super) fn run_with_host_message(
    code: impl Into<Vec<u8>>,
    host: &mut dyn Host,
    message: Message,
) -> TestInterpreter {
    run_with_host_message_tx_env_and_spec_config(
        code,
        host,
        message,
        TxEnv::default(),
        SpecId::HOMESTEAD,
        false,
    )
}

pub(super) fn run_with_host_message_tx_env_and_spec_config(
    code: impl Into<Vec<u8>>,
    host: &mut dyn Host,
    message: Message,
    tx_env: TxEnv,
    spec_id: SpecId,
    is_static: bool,
) -> TestInterpreter {
    let bytecode = Bytecode::new_legacy(Bytes::from(code.into()));
    let mut inner = Interpreter::new(bytecode, spec_id, tx_env, message);
    inner.is_static |= is_static;
    let err = inner.run(host);
    TestInterpreter { inner, err }
}

pub(super) fn run_with_host_and_spec_config(
    code: impl Into<Vec<u8>>,
    host: &mut dyn Host,
    spec_id: SpecId,
    is_static: bool,
    gas_limit: u64,
) -> TestInterpreter {
    let bytecode = Bytecode::new_legacy(Bytes::from(code.into()));
    let message = Message { gas_limit, ..Message::default() };
    let mut inner = Interpreter::new(bytecode, spec_id, TxEnv::default(), message);
    inner.is_static |= is_static;
    inner.gas = Gas::new(gas_limit);
    let err = inner.run(host);
    TestInterpreter { inner, err }
}

pub(super) trait ToWord {
    fn to_word(self) -> Word;
}

impl ToWord for Word {
    fn to_word(self) -> Word {
        self
    }
}

impl ToWord for i32 {
    fn to_word(self) -> Word {
        Word::from(self as u64)
    }
}

impl ToWord for u64 {
    fn to_word(self) -> Word {
        Word::from(self)
    }
}

impl ToWord for usize {
    fn to_word(self) -> Word {
        Word::from(self)
    }
}

pub(super) fn run_stack<T: ToWord, const N: usize>(inputs: [T; N], opcode: u8) -> TestInterpreter {
    let mut code = Vec::new();
    for input in inputs.into_iter().rev() {
        push(&mut code, input);
    }
    code.extend([opcode, op::STOP]);
    run(code)
}

pub(super) fn assert_stack_words(inputs: &[Word], opcode: u8, expected: &[Word]) {
    let mut code = Vec::new();
    for input in inputs.iter().rev() {
        push(&mut code, *input);
    }
    code.extend([opcode, op::STOP]);
    let interpreter = run(code);
    core::assert_matches!(interpreter.err, InstrStop::Stop);
    assert_eq!(interpreter.stack(), expected);
}

macro_rules! assert_stack {
    ($op:ident($($input:expr),* $(,)?), $expected:expr $(,)?) => {{
        let inputs = [$($crate::interpreter::Word::from($input)),*];
        let expected = [$crate::interpreter::Word::from($expected)];
        $crate::interpreter::instructions::tests::assert_stack_words(
            &inputs,
            $crate::interpreter::op::$op,
            &expected,
        );
    }};
}
pub(super) use assert_stack;

pub(super) fn push(code: &mut Vec<u8>, value: impl ToWord) {
    let value = value.to_word();
    if value.is_zero() {
        code.push(op::PUSH0);
        return;
    }

    let bytes = value.to_be_bytes::<32>();
    let start = bytes.iter().position(|&byte| byte != 0).unwrap();
    let len = bytes.len() - start;
    code.push(op::PUSH1 + len as u8 - 1);
    code.extend_from_slice(&bytes[start..]);
}
