use super::*;
use evm2::{
    BaseEvmConfigSelector, EvmFeatures, EvmTypes,
    bytecode::Bytecode as Evm2Bytecode,
    env::{BlockEnv as Evm2BlockEnv, TxEnv as Evm2TxEnv},
    evm::{
        AccountLoad as Evm2AccountLoad, SLoad as Evm2SLoad, SStore as Evm2SStore,
        SelfDestructResult as Evm2SelfDestructResult,
    },
    interpreter::{
        Gas, Host as Evm2Host, InstrStop, Interpreter as Evm2Interpreter, Message as Evm2Message,
        MessageResult as Evm2MessageResult,
    },
};
use evm2_jit_context::evm2_api;
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
    pub modify_ecx: Option<fn(&mut TestEvmContext<'_>)>,

    pub expected_return: InstrStop,
    pub expected_stack: &'a [U256],
    pub expected_memory: &'a [u8],
    pub expected_gas: u64,
    pub expected_output: Option<&'a [u8]>,
    pub assert_host: Option<fn(&TestHost)>,
    pub assert_ecx: Option<fn(&TestEvmContext<'_>)>,
}

#[cfg(feature = "__fuzzing")]
impl<'a> arbitrary::Arbitrary<'a> for TestCase<'a> {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let spec_id_range = 0..=(SpecId::OSAKA as u32 - 1);
        let spec_id = SpecId::try_from_u32(u.int_in_range(spec_id_range)?).unwrap_or(DEF_SPEC);

        let bytecode: &'a [u8] = u.arbitrary()?;

        Ok(Self::what_interpreter_says(bytecode, spec_id))
    }
}

impl Default for TestCase<'_> {
    fn default() -> Self {
        Self {
            bytecode: &[],
            spec_id: DEF_SPEC,
            is_static: false,
            gas_limit: DEF_GAS_LIMIT,
            inspect_stack: None,
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
pub static DEF_CODEMAP: OnceLock<HashMap<Address, Evm2Bytecode>> = OnceLock::new();
pub const OTHER_ADDR: Address = Address::repeat_byte(0x69);
pub const DEF_BN: U256 = uint!(500_U256);

pub const RETURN_WHAT_INTERPRETER_SAYS: InstrStop = InstrStop::PrecompileError;
pub const STACK_WHAT_INTERPRETER_SAYS: &[U256] =
    &[U256::from_be_slice(&GAS_WHAT_INTERPRETER_SAYS.to_be_bytes())];
pub const MEMORY_WHAT_INTERPRETER_SAYS: &[u8] = &GAS_WHAT_INTERPRETER_SAYS.to_be_bytes();
pub const GAS_WHAT_INTERPRETER_SAYS: u64 = 0x4682e332d6612de1;

pub type TestEvmContext<'a> = evm2_api::EvmContext<'a, TestEvmTypes>;
pub type TestEvmCompilerFn = evm2_api::EvmCompilerFn<TestEvmTypes>;

pub fn evm2_test_func(f: EvmCompilerFn) -> TestEvmCompilerFn {
    TestEvmCompilerFn::from_abi_compatible(f)
}

pub fn def_storage() -> &'static HashMap<U256, U256> {
    DEF_STORAGE.get_or_init(|| {
        let mut map = HashMap::default();
        map.insert(U256::from(0), U256::from(1));
        map.insert(U256::from(1), U256::from(2));
        map.insert(U256::from(69), U256::from(42));
        map
    })
}

pub fn def_codemap() -> &'static HashMap<Address, Evm2Bytecode> {
    DEF_CODEMAP.get_or_init(|| {
        let mut map = HashMap::default();
        map.insert(
            OTHER_ADDR,
            Evm2Bytecode::new_legacy(Bytes::from_static(&[
                op::PUSH1,
                0x69,
                op::PUSH1,
                0x42,
                op::ADD,
                op::STOP,
            ])),
        );
        map
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TestEvmTypes;

impl EvmTypes for TestEvmTypes {
    type ConfigSelector = BaseEvmConfigSelector;
    type SpecId = evm2::SpecId;
    type Tx = ();
    type MessageExt = ();
    type MessageResultExt = ();
    type TxEnvExt = ();
    type TxResultExt = ();
    type BlockEnvExt = ();
    type Host = TestHost;
}

/// Test host for codegen and runtime tests.
pub struct TestHost {
    pub storage: HashMap<U256, U256>,
    pub transient_storage: HashMap<U256, U256>,
    pub code_map: &'static HashMap<Address, Evm2Bytecode>,
    pub selfdestructs: Vec<(Address, Address)>,
    pub logs: Vec<Log>,
    evm2_spec_id: evm2::SpecId,
    evm2_block_env: Evm2BlockEnv<TestEvmTypes>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TestHost {
    pub fn new() -> Self {
        Self::with_spec(DEF_SPEC)
    }

    pub fn with_spec(spec_id: SpecId) -> Self {
        let env = def_env();
        Self {
            storage: def_storage().clone(),
            transient_storage: HashMap::default(),
            code_map: def_codemap(),
            selfdestructs: Vec::new(),
            logs: Vec::new(),
            evm2_spec_id: spec_id,
            evm2_block_env: Evm2BlockEnv {
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
            },
        }
    }
}

impl Evm2Host<TestEvmTypes> for TestHost {
    fn spec_id(&self) -> evm2::SpecId {
        self.evm2_spec_id
    }

    fn block_env(&mut self) -> &Evm2BlockEnv<TestEvmTypes> {
        &self.evm2_block_env
    }

    fn load_account(
        &mut self,
        address: &Address,
        load_code: bool,
        _skip_cold_load: bool,
    ) -> Result<Evm2AccountLoad, InstrStop> {
        let code = self.code_map.get(address);
        let bytecode =
            if load_code { code.cloned().unwrap_or_default() } else { Evm2Bytecode::default() };
        let balance = U256::from(address.0[19]);
        let code_hash =
            code.map(|code| keccak256(code.original_byte_slice())).unwrap_or(KECCAK_EMPTY);
        let is_empty = code.is_none() && balance.is_zero();

        Ok(Evm2AccountLoad {
            balance,
            code_hash,
            code: bytecode,
            exists: !is_empty,
            is_empty,
            is_cold: false,
            _non_exhaustive: (),
        })
    }

    fn target_is_empty_for_new_account_gas(
        &mut self,
        address: &Address,
        features: EvmFeatures,
    ) -> Result<bool, InstrStop> {
        let exists = self.code_map.contains_key(address) || !U256::from(address.0[19]).is_zero();
        if features.contains(EvmFeatures::EIP161) {
            return Ok(!exists);
        }
        Ok(false)
    }

    fn block_hash(&mut self, number: &U256) -> Result<Option<B256>, InstrStop> {
        Ok(Some((*number).into()))
    }

    fn sload(
        &mut self,
        _address: &Address,
        key: &U256,
        _skip_cold_load: bool,
    ) -> Result<Evm2SLoad, InstrStop> {
        Ok(Evm2SLoad {
            value: self.storage.get(key).copied().unwrap_or_default(),
            is_cold: false,
            _non_exhaustive: (),
        })
    }

    fn sstore(
        &mut self,
        _address: &Address,
        key: &U256,
        value: &U256,
        _skip_cold_load: bool,
    ) -> Result<Evm2SStore, InstrStop> {
        let original = self.storage.get(key).copied().unwrap_or_default();
        self.storage.insert(*key, *value);
        Ok(Evm2SStore {
            original_value: original,
            present_value: original,
            new_value: *value,
            is_cold: false,
            _non_exhaustive: (),
        })
    }

    fn tload(&mut self, _address: &Address, key: &U256) -> U256 {
        self.transient_storage.get(key).copied().unwrap_or_default()
    }

    fn tstore(&mut self, _address: &Address, key: &U256, value: &U256) {
        self.transient_storage.insert(*key, *value);
    }

    fn log(&mut self, log: Log) {
        self.logs.push(log);
    }

    fn execute_message(
        &mut self,
        _tx_env: &Evm2TxEnv<TestEvmTypes>,
        _bytecode: Evm2Bytecode,
        message: &mut Evm2Message<TestEvmTypes>,
        _caller_is_static: bool,
    ) -> Evm2MessageResult<TestEvmTypes> {
        Evm2MessageResult {
            stop: InstrStop::Return,
            gas: evm2::interpreter::GasTracker::new(message.gas_limit),
            ..Default::default()
        }
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        _skip_cold_load: bool,
    ) -> Result<Evm2SelfDestructResult, InstrStop> {
        self.selfdestructs.push((*contract, *target));
        Ok(Evm2SelfDestructResult {
            had_value: false,
            target_is_empty: false,
            previously_destroyed: false,
            ..Default::default()
        })
    }
}

pub fn with_evm_context<F: FnOnce(&mut TestEvmContext<'_>, &mut EvmStack, &mut usize) -> R, R>(
    bytecode: &[u8],
    spec_id: SpecId,
    f: F,
) -> R {
    with_evm_context_and_host(bytecode, spec_id, f).0
}

pub fn with_evm_context_and_host<
    F: FnOnce(&mut TestEvmContext<'_>, &mut EvmStack, &mut usize) -> R,
    R,
>(
    bytecode: &[u8],
    spec_id: SpecId,
    f: F,
) -> (R, TestHost) {
    let evm2_spec_id = spec_id;
    let config = <BaseEvmConfigSelector as evm2::EvmConfigSelector<TestEvmTypes>>::execution_config(
        evm2_spec_id,
    );
    let tx_env = Evm2TxEnv::<TestEvmTypes> {
        origin: def_env().tx.caller,
        gas_price: def_env().effective_gas_price(),
        chain_id: U256::from(def_env().cfg.chain_id),
        blob_hashes: def_env().tx.blob_hashes.iter().copied().map(Into::into).collect(),
        ..Default::default()
    };
    let message = Evm2Message::<TestEvmTypes> {
        destination: DEF_ADDR,
        caller: DEF_CALLER,
        input: Bytes::from_static(DEF_CD),
        value: DEF_VALUE,
        code_address: DEF_ADDR,
        gas_limit: DEF_GAS_LIMIT,
        ..Default::default()
    };
    let mut interpreter = Evm2Interpreter::<TestEvmTypes>::new(
        Evm2Bytecode::new_legacy(Bytes::copy_from_slice(bytecode)),
        &tx_env,
        &message,
        false,
    );
    let mut host = TestHost::with_spec(spec_id);
    interpreter.prepare_jit_run(&config, &mut host);

    let result = {
        let (mut ecx, stack, stack_len) =
            TestEvmContext::from_interpreter_with_stack(&mut interpreter, &mut host);
        f(&mut ecx, stack, stack_len)
    };
    (result, host)
}

pub fn set_test_dump<B: Backend>(compiler: &mut EvmCompiler<B>, module_path: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
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
    let TestCase {
        bytecode,
        spec_id,
        is_static,
        gas_limit,
        inspect_stack: _,
        modify_ecx,
        expected_return,
        expected_stack,
        expected_memory,
        expected_gas,
        expected_output,
        assert_host,
        assert_ecx,
    } = *test_case;

    let f = evm2_test_func(f);
    let (_, jit_host) = with_evm_context_and_host(bytecode, spec_id, |ecx, stack, stack_len| {
        if is_static {
            ecx.is_static = true;
        }
        if gas_limit != DEF_GAS_LIMIT {
            ecx.gas = Gas::new(gas_limit);
        }
        if let Some(modify_ecx) = modify_ecx {
            modify_ecx(ecx);
            ecx.refresh_memory_cache();
        }

        // Interpreter - run evm2 as the oracle
        let evm2_spec_id = spec_id;
        let config =
            <BaseEvmConfigSelector as evm2::EvmConfigSelector<TestEvmTypes>>::execution_config(
                evm2_spec_id,
            );
        let tx_env = Evm2TxEnv::<TestEvmTypes> {
            origin: def_env().tx.caller,
            gas_price: def_env().effective_gas_price(),
            chain_id: U256::from(def_env().cfg.chain_id),
            blob_hashes: def_env().tx.blob_hashes.iter().copied().map(Into::into).collect(),
            ..Default::default()
        };
        let message = Evm2Message::<TestEvmTypes> {
            destination: DEF_ADDR,
            caller: DEF_CALLER,
            input: Bytes::from_static(DEF_CD),
            value: DEF_VALUE,
            code_address: DEF_ADDR,
            gas_limit,
            ..Default::default()
        };
        let mut interpreter = Evm2Interpreter::<TestEvmTypes>::new(
            Evm2Bytecode::new_legacy(Bytes::copy_from_slice(bytecode)),
            &tx_env,
            &message,
            is_static,
        );
        let mut int_host = TestHost::with_spec(spec_id);
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

        if let Some(assert_host) = assert_host {
            assert_host(&int_host);
        }

        let actual_return = unsafe { f.call(stack, stack_len, ecx) };

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
                    MemDisplay(unsafe { &*ecx.memory }.as_slice()),
                    MemDisplay(expected_memory),
                    "memory mismatch"
                );
            }

            if !skip_jit_gas {
                assert_eq!(ecx.gas.spent(), expected_gas, "gas mismatch");
            }
        }

        if let Some(expected_output) = expected_output {
            assert_eq!(ecx.output.as_ref(), expected_output, "output mismatch");
        }

        if let Some(assert_ecx) = assert_ecx {
            assert_ecx(ecx);
        }
    });

    if let Some(assert_host) = assert_host {
        assert_host(&jit_host);
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
