use crate::{
    BaseEvmConfig, EvmConfig, EvmTypes, SpecId,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    evm::{AccountLoad, SLoad, SStore, SelfDestructResult},
    interpreter::{
        Gas, Host, InstrStop, Interpreter, Memory, Message, MessageKind, MessageResult, Stack,
        Word, op,
    },
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, Log, map::HashMap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TestTypes;

impl EvmTypes for TestTypes {
    type ConfigSelector = crate::BaseEvmConfigSelector;
    type SpecId = crate::SpecId;
    type Tx = ();
    type Host = TestHost;
    type Database = crate::evm::InMemoryDB;
    type Precompiles = crate::evm::precompile::NoPrecompiles;
}

#[derive(Debug)]
pub(crate) struct TestHost {
    pub(super) block: BlockEnv,
    pub(super) code_hash: B256,
    pub(super) code: Bytes,
    pub(super) exists: bool,
    pub(super) is_empty: bool,
    pub(super) is_touched: bool,
    pub(super) is_cold: bool,
    pub(super) storage: HashMap<(Address, Word), Word>,
    pub(super) original_storage: HashMap<(Address, Word), Word>,
    pub(super) transient_storage: HashMap<(Address, Word), Word>,
    pub(super) logs: Vec<Log>,
    pub(super) execute_result: MessageResult,
    pub(super) selfdestruct_result: SelfDestructResult,
    pub(super) calls: Vec<Message>,
    pub(super) call_static_flags: Vec<bool>,
    pub(super) selfdestructs: Vec<(Address, Address, bool)>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self {
            block: BlockEnv::default(),
            code_hash: B256::ZERO,
            code: Bytes::new(),
            exists: true,
            is_empty: false,
            is_touched: false,
            is_cold: false,
            storage: HashMap::default(),
            original_storage: HashMap::default(),
            transient_storage: HashMap::default(),
            logs: Vec::new(),
            execute_result: MessageResult { stop: InstrStop::Return, ..MessageResult::default() },
            selfdestruct_result: SelfDestructResult::default(),
            calls: Vec::new(),
            call_static_flags: Vec::new(),
            selfdestructs: Vec::new(),
        }
    }
}

impl Host for TestHost {
    fn spec_id(&self) -> SpecId {
        SpecId::OSAKA
    }

    fn block_env(&mut self) -> &BlockEnv {
        &self.block
    }

    fn load_account(
        &mut self,
        address: Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, InstrStop> {
        if skip_cold_load && self.is_cold {
            return Err(InstrStop::OutOfGas);
        }
        Ok(AccountLoad {
            balance: address.into_word().into(),
            code_hash: self.code_hash,
            code: if load_code { self.code.clone() } else { Bytes::new() },
            exists: self.exists,
            is_empty: self.is_empty,
            is_cold: self.is_cold,
        })
    }

    fn account_exists(&mut self, _address: Address) -> bool {
        self.exists || self.is_touched
    }

    fn block_hash(&mut self, number: Word) -> Option<B256> {
        Some(B256::with_last_byte(number.wrapping_to::<u8>()))
    }

    fn sload(
        &mut self,
        address: Address,
        key: Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, InstrStop> {
        if skip_cold_load && self.is_cold {
            return Err(InstrStop::OutOfGas);
        }
        Ok(SLoad {
            value: self.storage.get(&(address, key)).copied().unwrap_or_default(),
            is_cold: self.is_cold,
        })
    }

    fn sstore(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
        skip_cold_load: bool,
    ) -> Result<SStore, InstrStop> {
        if skip_cold_load && self.is_cold {
            return Err(InstrStop::OutOfGas);
        }
        let storage_key = (address, key);
        let present_value = self.storage.get(&storage_key).copied().unwrap_or_default();
        let original_value = *self.original_storage.entry(storage_key).or_insert(present_value);
        if value.is_zero() {
            self.storage.remove(&storage_key);
        } else {
            self.storage.insert(storage_key, value);
        }
        Ok(SStore { original_value, present_value, new_value: value, is_cold: self.is_cold })
    }

    fn tload(&mut self, address: Address, key: Word) -> Word {
        self.transient_storage.get(&(address, key)).copied().unwrap_or_default()
    }

    fn tstore(&mut self, address: Address, key: Word, value: Word) {
        self.transient_storage.insert((address, key), value);
    }

    fn log(&mut self, log: Log) {
        self.logs.push(log);
    }

    fn execute_message(
        &mut self,
        _tx_env: &TxEnv,
        _bytecode: Bytecode,
        message: &Message,
        caller_is_static: bool,
    ) -> MessageResult {
        self.call_static_flags.push(caller_is_static || message.kind == MessageKind::StaticCall);
        self.calls.push(message.clone());
        self.execute_result.clone()
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
    pub(super) stack: Box<[Word; Stack::CAPACITY]>,
    pub(super) stack_len: usize,
    pub(super) gas: Gas,
    pub(super) memory: Memory,
    pub(super) output: *const [u8],
    pub(super) err: InstrStop,
}

impl TestInterpreter {
    pub(super) fn stack(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.stack.as_ptr(), self.stack_len) }
    }

    pub(super) fn memory(&mut self, offset: usize, len: usize) -> &[u8] {
        self.memory.slice(offset, len)
    }

    pub(super) fn gas_remaining(&self) -> u64 {
        self.gas.remaining()
    }

    pub(super) fn gas_refunded(&self) -> i64 {
        self.gas.refunded()
    }

    pub(super) fn state_gas_spent(&self) -> u64 {
        self.gas.state_gas_spent()
    }

    pub(super) fn output(&self) -> &[u8] {
        // SAFETY: The output pointer is created from memory owned by this test interpreter, or is
        // the shared empty slice pointer.
        unsafe { &*self.output }
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
            spec_id: SpecId::OSAKA,
            tx_env: TxEnv::default(),
            message: Message { gas_limit: 10_000, ..Message::default() },
            gas_limit: 10_000,
            return_data: Bytes::new(),
        }
    }
}

pub(super) fn run(config: RunConfig<'_>) -> TestInterpreter {
    crate::spec_to_generic!(config.spec_id, |BASE_SPEC_ID| {
        run_with_config::<BaseEvmConfig<BASE_SPEC_ID>>(config)
    })
}

fn run_with_config<C: EvmConfig<TestTypes>>(config: RunConfig<'_>) -> TestInterpreter {
    let RunConfig { code, host, spec_id: _, tx_env, mut message, gas_limit, return_data } = config;
    let bytecode = Bytecode::new_legacy(Bytes::from(code));
    message.gas_limit = gas_limit;
    let mut inner = Interpreter::<TestTypes>::new(bytecode, &tx_env, &message, false);
    inner.return_data = return_data;
    let mut default_host = TestHost::default();
    let host = host.unwrap_or(&mut default_host);
    let err = inner.run::<C>(host);
    let stack_len = inner.stack_len();
    TestInterpreter {
        stack: inner.stack,
        stack_len,
        gas: inner.gas,
        memory: inner.memory,
        output: inner.output,
        err,
    }
}

pub(crate) trait ToWord {
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
pub(crate) use assert_stack;

pub(crate) fn push(code: &mut Vec<u8>, value: impl ToWord) {
    let value = value.to_word();
    if value.is_zero() {
        code.extend([op::PUSH1, 0]);
        return;
    }

    let bytes = value.to_be_bytes::<32>();
    let start = bytes.iter().position(|&byte| byte != 0).unwrap();
    let len = bytes.len() - start;
    code.push(op::PUSH1 + len as u8 - 1);
    code.extend_from_slice(&bytes[start..]);
}
