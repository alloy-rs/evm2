use crate::{
    AccountLoad, EvmConfig, SelfDestructResult,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    interpreter::{Host, InstrStop, Interpreter, Message, MessageKind, SpecId, Stack, Word, op},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, Log};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::interpreter) struct TestConfig<const SPEC: u8 = { SpecId::OSAKA as u8 }>;

impl<const SPEC: u8> EvmConfig for TestConfig<SPEC> {
    type Tx = ();
    type Host = TestHost;

    const SPEC_ID: SpecId = match SpecId::try_from_u8(SPEC) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    };
}

#[derive(Debug)]
pub(in crate::interpreter) struct TestHost {
    pub(super) block: BlockEnv,
    pub(super) code_hash: B256,
    pub(super) code: Bytes,
    pub(super) is_empty: bool,
    pub(super) is_cold: bool,
    pub(super) storage: HashMap<Word, Word>,
    pub(super) transient_storage: HashMap<Word, Word>,
    pub(super) logs: Vec<Log>,
    pub(super) execute_result: Result<Word, InstrStop>,
    pub(super) selfdestruct_result: SelfDestructResult,
    pub(super) calls: Vec<Message>,
    pub(super) selfdestructs: Vec<(Address, Address, bool)>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self {
            block: BlockEnv::default(),
            code_hash: B256::ZERO,
            code: Bytes::new(),
            is_empty: false,
            is_cold: false,
            storage: HashMap::new(),
            transient_storage: HashMap::new(),
            logs: Vec::new(),
            execute_result: Ok(Word::from(1)),
            selfdestruct_result: SelfDestructResult::default(),
            calls: Vec::new(),
            selfdestructs: Vec::new(),
        }
    }
}

impl Host for TestHost {
    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Word,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        if skip_cold_load && self.is_cold {
            return Err(InstrStop::OutOfGas);
        }
        Ok(AccountLoad {
            balance: address,
            code_hash: self.code_hash,
            code: if load_code { self.code.clone() } else { Bytes::new() },
            is_empty: self.is_empty,
            is_cold: self.is_cold,
        })
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

    fn execute_message(
        &mut self,
        _tx_env: TxEnv,
        _bytecode: Bytecode,
        message: Message,
    ) -> Result<Word, InstrStop> {
        self.calls.push(message);
        self.execute_result
    }

    fn selfdestruct(
        &mut self,
        contract: Address,
        target: Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, InstrStop> {
        self.selfdestructs.push((contract, target, skip_cold_load));
        Ok(self.selfdestruct_result)
    }
}

pub(super) struct TestInterpreter {
    pub(super) pc: usize,
    pub(super) stack: Box<[Word; Stack::CAPACITY]>,
    pub(super) stack_len: usize,
    pub(super) gas: crate::interpreter::Gas,
    pub(super) memory: crate::interpreter::Memory,
    pub(super) err: InstrStop,
}

impl TestInterpreter {
    pub(super) fn stack(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.stack.as_ptr(), self.stack_len) }
    }

    pub(super) fn memory(&mut self, offset: usize, len: usize) -> &[u8] {
        self.memory.slice(offset, len).unwrap()
    }

    pub(super) fn gas_remaining(&self) -> u64 {
        self.gas.remaining()
    }
}

pub(super) struct RunConfig<'a> {
    pub(super) code: Vec<u8>,
    pub(super) host: Option<&'a mut TestHost>,
    pub(super) spec_id: SpecId,
    pub(super) tx_env: TxEnv,
    pub(super) message: Message,
    pub(super) gas_limit: u64,
    pub(super) return_data: Bytes,
}

impl<'a> RunConfig<'a> {
    pub(super) fn new(code: impl Into<Vec<u8>>) -> Self {
        Self { code: code.into(), ..Self::default() }
    }

    pub(super) fn host(mut self, host: &'a mut TestHost) -> Self {
        self.host = Some(host);
        self
    }

    pub(super) const fn spec(mut self, spec_id: SpecId) -> Self {
        self.spec_id = spec_id;
        self
    }

    pub(super) fn tx_env(mut self, tx_env: TxEnv) -> Self {
        self.tx_env = tx_env;
        self
    }

    pub(super) fn message(mut self, message: Message) -> Self {
        self.message = message;
        self
    }

    pub(super) fn staticcall(mut self) -> Self {
        self.message.kind = MessageKind::StaticCall;
        self
    }

    pub(super) const fn gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    pub(super) fn return_data(mut self, return_data: Bytes) -> Self {
        self.return_data = return_data;
        self
    }
}

impl Default for RunConfig<'_> {
    fn default() -> Self {
        Self {
            code: Vec::new(),
            host: None,
            spec_id: SpecId::HOMESTEAD,
            tx_env: TxEnv::default(),
            message: Message { gas_limit: 10_000, ..Message::default() },
            gas_limit: 10_000,
            return_data: Bytes::new(),
        }
    }
}

pub(super) fn run(config: RunConfig<'_>) -> TestInterpreter {
    macro_rules! run_with_spec {
        ($config:expr, $spec:expr, $($spec_id:ident),* $(,)?) => {
            match $spec {
                $(
                    SpecId::$spec_id => run_with_config::<TestConfig<{ SpecId::$spec_id as u8 }>>($config),
                )*
            }
        };
    }

    run_with_spec!(
        config,
        config.spec_id,
        FRONTIER,
        FRONTIER_THAWING,
        HOMESTEAD,
        DAO_FORK,
        TANGERINE,
        SPURIOUS_DRAGON,
        BYZANTIUM,
        CONSTANTINOPLE,
        PETERSBURG,
        ISTANBUL,
        MUIR_GLACIER,
        BERLIN,
        LONDON,
        ARROW_GLACIER,
        GRAY_GLACIER,
        MERGE,
        SHANGHAI,
        CANCUN,
        PRAGUE,
        OSAKA,
        AMSTERDAM,
    )
}

fn run_with_config<C: EvmConfig<Tx = (), Host = TestHost>>(
    config: RunConfig<'_>,
) -> TestInterpreter {
    let RunConfig { code, host, spec_id: _, tx_env, mut message, gas_limit, return_data } = config;
    let bytecode = Bytecode::new_legacy(Bytes::from(code));
    message.gas_limit = gas_limit;
    let mut inner = Interpreter::new(bytecode, tx_env, message);
    inner.return_data = return_data;
    let mut default_host = TestHost::default();
    let host = host.unwrap_or(&mut default_host);
    let err = inner.run::<C>(host);
    TestInterpreter {
        pc: inner.pc,
        stack: inner.stack,
        stack_len: inner.stack_len,
        gas: inner.gas,
        memory: inner.memory,
        err,
    }
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
    run(RunConfig::new(code))
}

pub(super) fn assert_stack_words(inputs: &[Word], opcode: u8, expected: &[Word]) {
    let mut code = Vec::new();
    for input in inputs.iter().rev() {
        push(&mut code, *input);
    }
    code.extend([opcode, op::STOP]);
    let interpreter = run(RunConfig::new(code));
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
