use super::*;
use evm2::{
    BaseEvmConfigSelector, BaseEvmTypes, Evm, Precompiles,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    ethereum::ethereum_tx_registry,
    evm::{AccountInfo, InMemoryDB},
    interpreter::{Gas, InstrStop, Interpreter, Message},
};
use evm2_jit_context::EvmContext;
use similar_asserts::assert_eq;
use std::{fmt, path::Path, sync::OnceLock};

/// Initializes `tracing_subscriber` for tests. Safe to call multiple times.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt::try_init();
}

/// Default test environment struct for test expected values.
#[derive(Clone, Debug)]
pub struct DefEnv {
    pub tx: DefTx,
    pub block: DefBlock,
    pub cfg: DefCfg,
}

#[derive(Clone, Debug)]
pub struct DefTx {
    pub caller: Address,
    pub blob_hashes: Vec<B256>,
}

#[derive(Clone, Copy, Debug)]
pub struct DefBlock {
    pub coinbase: Address,
    pub timestamp: U256,
    pub number: U256,
    pub difficulty: U256,
    pub prevrandao: Option<B256>,
    pub gas_limit: U256,
    pub basefee: U256,
}

impl DefBlock {
    pub fn get_blob_gasprice(&self) -> Option<u64> {
        Some(0) // Default blob gas price for tests
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DefCfg {
    pub chain_id: u64,
}

impl DefEnv {
    pub fn effective_gas_price(&self) -> U256 {
        U256::from(0x4567)
    }
}

/// Returns the default test environment.
pub fn def_env() -> DefEnv {
    DefEnv {
        tx: DefTx {
            caller: Address::repeat_byte(0xcc),
            blob_hashes: vec![B256::repeat_byte(0x01), B256::repeat_byte(0x02)],
        },
        block: DefBlock {
            coinbase: Address::repeat_byte(0xcb),
            timestamp: U256::from(0x1234),
            number: DEF_BN,
            difficulty: U256::from(0xcdef),
            prevrandao: Some(B256::from(U256::from(0x0123))),
            gas_limit: U256::from(0x5678),
            basefee: U256::from(0x1231),
        },
        cfg: DefCfg { chain_id: 69 },
    }
}

/// Memory gas calculation: `num_words * 3 + num_words**2 / 512`.
pub fn memory_gas_cost(num_words: usize) -> u64 {
    (num_words as u64) * 3 + (num_words as u64) * (num_words as u64) / 512
}

pub struct TestCase<'a> {
    pub bytecode: &'a [u8],
    pub spec_id: SpecId,
    pub is_static: bool,
    pub gas_limit: u64,

    /// Override `inspect_stack` on the compiler. `None` uses the default (`true`).
    pub inspect_stack: Option<bool>,
    pub modify_message: Option<fn(&mut Message<BaseEvmTypes>)>,
    pub modify_ecx: Option<fn(&mut EvmContext<'_>)>,

    pub expected_return: InstrStop,
    pub expected_stack: &'a [U256],
    pub expected_memory: &'a [u8],
    pub expected_gas: u64,
    pub expected_output: Option<&'a [u8]>,
    pub assert_host: Option<fn(&HostState)>,
    pub assert_ecx: Option<fn(&EvmContext<'_>)>,
}

impl Default for TestCase<'_> {
    fn default() -> Self {
        Self {
            bytecode: &[],
            spec_id: DEF_SPEC,
            is_static: false,
            gas_limit: DEF_GAS_LIMIT,
            inspect_stack: None,
            modify_message: None,
            modify_ecx: None,
            expected_return: InstrStop::Stop,
            expected_stack: &[],
            expected_memory: &[],
            expected_gas: 0,
            expected_output: None,
            assert_host: None,
            assert_ecx: None,
        }
    }
}

impl fmt::Debug for TestCase<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestCase")
            .field("bytecode", &format_bytecode(self.bytecode, self.spec_id))
            .field("spec_id", &self.spec_id)
            .field("inspect_stack", &self.inspect_stack)
            .field("modify_message", &self.modify_message.is_some())
            .field("modify_ecx", &self.modify_ecx.is_some())
            .field("expected_return", &self.expected_return)
            .field("expected_stack", &self.expected_stack)
            .field("expected_memory", &MemDisplay(self.expected_memory))
            .field("expected_gas", &self.expected_gas)
            .field("expected_output", &self.expected_output)
            .field("assert_host", &self.assert_host.is_some())
            .field("assert_ecx", &self.assert_ecx.is_some())
            .finish()
    }
}

impl<'a> TestCase<'a> {
    pub fn what_interpreter_says(bytecode: &'a [u8], spec_id: SpecId) -> Self {
        Self {
            bytecode,
            spec_id,
            is_static: false,
            gas_limit: DEF_GAS_LIMIT,
            inspect_stack: None,
            modify_message: None,
            modify_ecx: None,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            expected_output: None,
            assert_host: None,
            assert_ecx: None,
        }
    }
}

// Default values.
pub const DEF_SPEC: SpecId = SpecId::CANCUN;
pub static DEF_OPINFOS: std::sync::LazyLock<&'static [OpcodeInfo; 256]> =
    std::sync::LazyLock::new(|| op_info_map(DEF_SPEC));

pub const DEF_GAS_LIMIT: u64 = 100_000;
pub const DEF_GAS_LIMIT_U256: U256 = U256::from_le_slice(&DEF_GAS_LIMIT.to_le_bytes());

/// Default code address.
pub const DEF_ADDR: Address = Address::repeat_byte(0xba);
pub const DEF_CALLER: Address = Address::repeat_byte(0xca);
pub static DEF_CD: &[u8] = &[0xaa; 64];
pub static DEF_RD: &[u8] = &[0xbb; 64];
pub static DEF_DATA: &[u8] = &[0xcc; 64];
pub const DEF_VALUE: U256 = uint!(123_456_789_U256);
pub static DEF_STORAGE: OnceLock<HashMap<U256, U256>> = OnceLock::new();
pub static DEF_CODEMAP: OnceLock<HashMap<Address, Bytecode>> = OnceLock::new();
pub const OTHER_ADDR: Address = Address::repeat_byte(0x69);
pub const DEF_BN: U256 = uint!(500_U256);

pub const RETURN_WHAT_INTERPRETER_SAYS: InstrStop = InstrStop::PrecompileError;
pub const STACK_WHAT_INTERPRETER_SAYS: &[U256] =
    &[U256::from_be_slice(&GAS_WHAT_INTERPRETER_SAYS.to_be_bytes())];
pub const MEMORY_WHAT_INTERPRETER_SAYS: &[u8] = &GAS_WHAT_INTERPRETER_SAYS.to_be_bytes();
pub const GAS_WHAT_INTERPRETER_SAYS: u64 = 0x4682e332d6612de1;

pub fn def_storage() -> &'static HashMap<U256, U256> {
    DEF_STORAGE.get_or_init(|| {
        let mut map = HashMap::default();
        map.insert(U256::from(0), U256::from(1));
        map.insert(U256::from(1), U256::from(2));
        map.insert(U256::from(69), U256::from(42));
        map
    })
}

pub fn def_codemap() -> &'static HashMap<Address, Bytecode> {
    DEF_CODEMAP.get_or_init(|| {
        let mut map = HashMap::default();
        map.insert(
            OTHER_ADDR,
            Bytecode::new_legacy(Bytes::from_static(&[
                op::PUSH1,
                0x69,
                op::PUSH1,
                0x42,
                op::ADD,
                op::STOP,
            ])),
        );
        map.insert(
            Address::with_last_byte(0x68),
            Bytecode::new_legacy(Bytes::from_static(&[
                op::CALLVALUE,
                op::PUSH0,
                op::MSTORE,
                op::PUSH1,
                0x20,
                op::PUSH0,
                op::RETURN,
            ])),
        );
        map
    })
}

fn def_block_env() -> BlockEnv<BaseEvmTypes> {
    let env = def_env();
    BlockEnv {
        number: env.block.number,
        beneficiary: env.block.coinbase,
        timestamp: env.block.timestamp,
        gas_limit: env.block.gas_limit,
        basefee: env.block.basefee,
        difficulty: env.block.difficulty,
        prevrandao: U256::from(0x0123),
        blob_basefee: U256::ZERO,
        slot_num: U256::ZERO,
        ..Default::default()
    }
}

fn def_tx_env() -> TxEnv<BaseEvmTypes> {
    let env = def_env();
    TxEnv::<BaseEvmTypes> {
        origin: env.tx.caller,
        gas_price: env.effective_gas_price(),
        chain_id: U256::from(env.cfg.chain_id),
        blob_hashes: env.tx.blob_hashes.iter().copied().map(Into::into).collect(),
        ..Default::default()
    }
}

fn def_message(gas_limit: u64) -> Message<BaseEvmTypes> {
    Message::<BaseEvmTypes> {
        destination: DEF_ADDR,
        caller: DEF_CALLER,
        input: Bytes::from_static(DEF_CD),
        value: DEF_VALUE,
        code_address: DEF_ADDR,
        gas_limit,
        ..Default::default()
    }
}

fn def_database() -> InMemoryDB {
    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &DEF_ADDR,
        AccountInfo { balance: U256::from(DEF_ADDR.0[19]), ..Default::default() },
    );
    database.insert_account_info(
        &DEF_CALLER,
        AccountInfo { balance: U256::from(DEF_CALLER.0[19]), ..Default::default() },
    );
    database.insert_account_info(
        &Address::with_last_byte(0x69),
        AccountInfo { balance: U256::from(0x69), ..Default::default() },
    );
    for (address, code) in def_codemap() {
        database.insert_account_info(
            address,
            AccountInfo {
                balance: U256::from(address.0[19]),
                code: Some(code.clone()),
                ..Default::default()
            },
        );
    }
    for (key, value) in def_storage() {
        database.insert_account_storage(&DEF_ADDR, key, value);
    }
    for number in [DEF_BN - U256::from(1), DEF_BN - U256::from(255), DEF_BN - U256::from(256)] {
        database.insert_block_hash(&number, &number.into());
    }
    database
}

/// Host state snapshot for codegen assertions.
pub struct HostState {
    pub storage: HashMap<U256, U256>,
    pub transient_storage: HashMap<U256, U256>,
    pub logs: Vec<Log>,
}

impl HostState {
    fn from_evm(evm: &mut Evm<BaseEvmTypes>) -> Self {
        let keys = [
            U256::from(0),
            U256::from(1),
            U256::from(69),
            U256::from(70),
            U256::from(200),
            U256::from(0xff),
        ];
        let mut storage = HashMap::default();
        for key in keys {
            if let Some(value) = evm.state().get_storage(&DEF_ADDR, &key)
                && !value.is_zero()
            {
                storage.insert(key, value);
            }
        }

        let mut transient_storage = HashMap::default();
        for key in keys {
            let value = evm.state_mut().tload(&DEF_ADDR, &key);
            if !value.is_zero() {
                transient_storage.insert(key, value);
            }
        }

        Self { storage, transient_storage, logs: evm.logs().to_vec() }
    }
}

fn prepare_host(spec_id: SpecId) -> Evm<BaseEvmTypes> {
    let mut evm = Evm::<BaseEvmTypes>::new(
        spec_id,
        def_block_env(),
        ethereum_tx_registry(spec_id),
        def_database(),
        Precompiles::base(spec_id),
    );
    for address in [Address::ZERO, DEF_ADDR, DEF_CALLER, OTHER_ADDR, Address::with_last_byte(0x69)]
    {
        evm.state_mut().prewarm(&address);
    }
    for key in [
        U256::from(0),
        U256::from(1),
        U256::from(69),
        U256::from(70),
        U256::from(200),
        U256::from(0xff),
    ] {
        evm.state_mut().prewarm_storage_slot(&DEF_ADDR, key);
    }
    evm
}

pub fn with_evm_context<F: FnOnce(&mut EvmContext<'_>, &mut EvmStack, &mut usize) -> R, R>(
    bytecode: &[u8],
    spec_id: SpecId,
    f: F,
) -> R {
    with_evm_context_and_host(bytecode, spec_id, f).0
}

fn with_evm_context_and_host_mut<
    F: FnOnce(&mut EvmContext<'_>, &mut EvmStack, &mut usize) -> R,
    R,
>(
    bytecode: &[u8],
    spec_id: SpecId,
    host: &mut Evm<BaseEvmTypes>,
    modify_message: Option<fn(&mut Message<BaseEvmTypes>)>,
    f: F,
) -> R {
    let config =
        <BaseEvmConfigSelector as evm2::EvmConfigSelector<BaseEvmTypes>>::execution_config(spec_id);
    let tx_env = def_tx_env();
    let mut message = def_message(DEF_GAS_LIMIT);
    if let Some(modify_message) = modify_message {
        modify_message(&mut message);
    }
    let mut interpreter = Interpreter::<BaseEvmTypes>::new(
        Bytecode::new_legacy(Bytes::copy_from_slice(bytecode)),
        &tx_env,
        &message,
    );
    interpreter.prepare_jit_run(&config, host);

    let (mut ecx, stack, stack_len) = EvmContext::from_interpreter_with_stack(&mut interpreter);
    f(&mut ecx, stack, stack_len)
}

pub fn with_evm_context_and_host<
    F: FnOnce(&mut EvmContext<'_>, &mut EvmStack, &mut usize) -> R,
    R,
>(
    bytecode: &[u8],
    spec_id: SpecId,
    f: F,
) -> (R, HostState) {
    with_evm_context_and_host_modified(bytecode, spec_id, None, f)
}

fn with_evm_context_and_host_modified<
    F: FnOnce(&mut EvmContext<'_>, &mut EvmStack, &mut usize) -> R,
    R,
>(
    bytecode: &[u8],
    spec_id: SpecId,
    modify_message: Option<fn(&mut Message<BaseEvmTypes>)>,
    f: F,
) -> (R, HostState) {
    let mut host = prepare_host(spec_id);
    let result = with_evm_context_and_host_mut(bytecode, spec_id, &mut host, modify_message, f);
    let host = HostState::from_evm(&mut host);
    (result, host)
}

pub fn set_test_dump<B: Backend>(compiler: &mut EvmCompiler<B>, module_path: &str) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().parent().unwrap();
    let mut dump_path = root.to_path_buf();
    dump_path.push("target");
    dump_path.push("tests_dump");
    // Skip `evm2_jit::tests`.
    dump_path.extend(module_path.split("::").skip(2));
    dump_path.push(format!("{:?}", compiler.opt_level()));
    compiler.set_dump_to(Some(dump_path));
}

pub fn run_test_case<B: Backend>(test_case: &TestCase<'_>, compiler: &mut EvmCompiler<B>) {
    let TestCase { bytecode, spec_id, .. } = *test_case;
    compiler.inspect_stack(test_case.inspect_stack.unwrap_or(true));
    // compiler.debug_assertions(false);
    let f = unsafe { compiler.jit("test", bytecode, spec_id) }.unwrap();
    run_compiled_test_case(test_case, f);
}

fn run_compiled_test_case(test_case: &TestCase<'_>, f: EvmCompilerFn) {
    let TestCase { bytecode, spec_id, modify_message, assert_host, .. } = *test_case;

    let (_, host_after_jit) = with_evm_context_and_host_modified(
        bytecode,
        spec_id,
        modify_message,
        |ecx, stack, stack_len| {
            run_compiled_test_case_with_context(test_case, f, ecx, stack, stack_len);
        },
    );

    if let Some(assert_host) = assert_host {
        assert_host(&host_after_jit);
    }
}

fn run_compiled_test_case_with_context(
    test_case: &TestCase<'_>,
    f: EvmCompilerFn,
    ecx: &mut EvmContext<'_>,
    stack: &mut EvmStack,
    stack_len: &mut usize,
) {
    let TestCase {
        bytecode,
        spec_id,
        is_static,
        gas_limit,
        inspect_stack: _,
        modify_message,
        modify_ecx,
        expected_return,
        expected_stack,
        expected_memory,
        expected_gas,
        expected_output,
        assert_host: _,
        assert_ecx,
    } = *test_case;

    if is_static {
        ecx.set_static_for_jit(true);
    }
    if gas_limit != DEF_GAS_LIMIT {
        ecx.gas = Gas::new(gas_limit);
    }
    if let Some(modify_ecx) = modify_ecx {
        modify_ecx(ecx);
        ecx.refresh_memory_cache();
    }

    // Interpreter - run evm2 as the oracle
    let config =
        <BaseEvmConfigSelector as evm2::EvmConfigSelector<BaseEvmTypes>>::execution_config(spec_id);
    let tx_env = def_tx_env();
    let mut message = def_message(gas_limit);
    if let Some(modify_message) = modify_message {
        modify_message(&mut message);
    }
    message.caller_is_static = is_static;
    let mut interpreter = Interpreter::<BaseEvmTypes>::new(
        Bytecode::new_legacy(Bytes::copy_from_slice(bytecode)),
        &tx_env,
        &message,
    );
    let mut int_host = prepare_host(spec_id);
    let int_stop = interpreter.run(&config, &mut int_host);
    let int_result = int_stop;
    let interpreter_output = interpreter.output();

    let mut expected_return = expected_return;
    if expected_return == RETURN_WHAT_INTERPRETER_SAYS {
        expected_return = int_result;
    } else if modify_ecx.is_none() {
        // Only check interpreter return if modify_ecx is not set.
        // When modify_ecx is used, it only modifies the JIT context, not the interpreter's
        // input, so the interpreter may return a different result.
        assert!(
            instruction_results_match_for_oracle(int_result, expected_return),
            "interpreter return value mismatch: {int_result:?} != {expected_return:?}"
        );
    }

    // When modify_ecx is set, the interpreter runs with different inputs than the JIT,
    // so we cannot use interpreter results as expected values or compare against them.
    let skip_interpreter_checks = modify_ecx.is_some() || expected_return.is_halt();
    let interpreter_stack_ref = (!skip_interpreter_checks).then(|| interpreter.stack());
    let interpreter_stack = interpreter_stack_ref.as_ref().map(|stack| stack.as_slice());

    let mut expected_stack = expected_stack;
    if expected_stack == STACK_WHAT_INTERPRETER_SAYS {
        if skip_interpreter_checks {
            expected_stack = &[]; // Will skip comparison below
        } else {
            expected_stack = interpreter_stack.unwrap();
        }
    } else if !skip_interpreter_checks {
        assert_eq!(interpreter_stack.unwrap(), expected_stack, "interpreter stack mismatch");
    }

    let interpreter_memory = interpreter.memory_ref().as_slice();
    let mut expected_memory = expected_memory;
    if expected_memory == MEMORY_WHAT_INTERPRETER_SAYS {
        if skip_interpreter_checks {
            expected_memory = &[]; // Will skip comparison below
        } else {
            expected_memory = interpreter_memory;
        }
    } else if !skip_interpreter_checks {
        assert_eq!(
            MemDisplay(interpreter_memory),
            MemDisplay(expected_memory),
            "interpreter memory mismatch"
        );
    }

    let mut expected_gas = expected_gas;
    if expected_gas == GAS_WHAT_INTERPRETER_SAYS {
        if skip_interpreter_checks {
            expected_gas = 0; // Will skip comparison below
        } else {
            expected_gas = interpreter.gas().spent();
        }
    } else if !skip_interpreter_checks {
        assert_eq!(interpreter.gas().spent(), expected_gas, "interpreter gas mismatch");
    }

    let expected_output = if let Some(expected_output) = expected_output {
        if !skip_interpreter_checks {
            assert_eq!(interpreter_output, expected_output, "interpreter output mismatch");
        }
        Some(expected_output)
    } else if skip_interpreter_checks {
        None
    } else {
        Some(interpreter_output)
    };

    // Track whether we should skip JIT stack/gas/memory comparisons
    let skip_jit_stack =
        skip_interpreter_checks && test_case.expected_stack == STACK_WHAT_INTERPRETER_SAYS;
    let skip_jit_memory =
        skip_interpreter_checks && test_case.expected_memory == MEMORY_WHAT_INTERPRETER_SAYS;
    let skip_jit_gas =
        skip_interpreter_checks && test_case.expected_gas == GAS_WHAT_INTERPRETER_SAYS;

    let actual_return = unsafe { f.call(ecx, stack, stack_len) };

    if matches!(
        actual_return,
        // We can have a stack overflow/underflow before other error codes due to sections.
        |InstrStop::StackOverflow| InstrStop::StackUnderflow
            // Any OOG is equivalent. We skip `InvalidOperand` sometimes.
            | InstrStop::OutOfGas
            | InstrStop::MemoryOOG
            | InstrStop::MemoryLimitOOG
            | InstrStop::InvalidOperandOOG
    ) {
        assert_eq!(
            actual_return.is_halt(),
            expected_return.is_halt(),
            "return value mismatch: {actual_return:?} != {expected_return:?}"
        );
    } else {
        assert_eq!(actual_return, expected_return, "return value mismatch");
    }

    let actual_stack =
        unsafe { stack.as_slice(*stack_len).iter().map(|x| x.to_u256()).collect::<Vec<_>>() };

    // On EVM halt all available gas is consumed, so resulting stack, memory, and gas do not
    // matter. We do less work than the interpreter by bailing out earlier due to sections.
    if !actual_return.is_halt() {
        if !skip_jit_stack {
            assert_eq!(actual_stack, *expected_stack, "stack mismatch");
        }

        if !skip_jit_memory {
            assert_eq!(
                MemDisplay(ecx.memory().as_slice()),
                MemDisplay(expected_memory),
                "memory mismatch"
            );
        }

        if !skip_jit_gas {
            assert_eq!(ecx.gas.spent(), expected_gas, "gas mismatch");
        }
    }

    if let Some(expected_output) = expected_output {
        assert_eq!(ecx.interpreter().output(), expected_output, "output mismatch");
    }

    if let Some(assert_ecx) = assert_ecx {
        assert_ecx(ecx);
    }
}

fn instruction_results_match_for_oracle(actual: InstrStop, expected: InstrStop) -> bool {
    actual == expected
        || matches!(
            (actual, expected),
            (InstrStop::InvalidOpcode, InstrStop::NotActivated)
                | (InstrStop::StackUnderflow, InstrStop::StackOverflow)
        )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MemDisplay<'a>(&'a [u8]);
impl fmt::Debug for MemDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chunks = self.0.chunks(32).map(hex::encode_prefixed);
        f.debug_list().entries(chunks).finish()
    }
}
