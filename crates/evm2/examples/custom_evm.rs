//! Defines a custom EVM configuration factory.

use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, Bytes};
use evm2::{
    BaseEvmConfig, Evm, EvmConfig, EvmConfigFactory, EvmTypes, EvmVersion, SpecId, Version,
    base_run_interpreter,
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{InMemoryDB, RunInterpreterFn, precompile::NoPrecompiles},
    interpreter::{Host, InstrStop, Instruction, Message, Word, op},
    registry::{HandlerResult, TxRegistry, TxRequest},
};
use evm2_macros::instruction;

const CUSTOM_OPCODE: u8 = 0x0c;
const CUSTOM_OPCODE_GAS: u16 = 7;
const CUSTOM_TX_TYPE: u8 = 0x7f;

#[derive(Clone, Copy, Debug)]
enum CustomSpecId {
    MainnetOsaka,
    CustomOsaka,
}

impl From<CustomSpecId> for SpecId {
    fn from(spec_id: CustomSpecId) -> Self {
        match spec_id {
            CustomSpecId::MainnetOsaka | CustomSpecId::CustomOsaka => Self::OSAKA,
        }
    }
}

#[derive(Debug)]
struct CustomTypes;

impl EvmTypes for CustomTypes {
    type ConfigFactory = CustomConfigFactory;
    type SpecId = CustomSpecId;
    type Tx = CustomTx;
    type Host = Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = NoPrecompiles;
}

static CUSTOM_VERSION: Version = {
    let mut version = Version::new_base(SpecId::OSAKA);
    version.static_gas_table.set(CUSTOM_OPCODE, CUSTOM_OPCODE_GAS);
    version
};

struct CustomConfig<const SPEC_ID: u8>(());

impl<const SPEC_ID: u8> EvmConfig for CustomConfig<SPEC_ID> {
    const VERSION: &'static Version = &CUSTOM_VERSION;
}

struct CustomConfigFactory(());

static CUSTOM_EVM_VERSION: EvmVersion<CustomTypes> = {
    let mut version =
        EvmVersion::<CustomTypes>::new_base::<CustomConfig<{ SpecId::OSAKA as u8 }>>();
    version.instruction_impls.set(
        CUSTOM_OPCODE,
        Some(
            <custom<CustomTypes> as Instruction<CustomTypes>>::execute::<
                CustomConfig<{ SpecId::OSAKA as u8 }>,
            >,
        ),
    );
    version
};

impl EvmConfigFactory<CustomTypes> for CustomConfigFactory {
    type Config<const SPEC_ID: u8> = CustomConfig<SPEC_ID>;

    fn run_interpreter(spec_id: CustomSpecId) -> RunInterpreterFn<CustomTypes> {
        match spec_id {
            CustomSpecId::MainnetOsaka => run_base_osaka,
            CustomSpecId::CustomOsaka => base_run_interpreter::<CustomTypes, Self>(spec_id.into()),
        }
    }

    fn version(spec_id: CustomSpecId) -> &'static Version {
        match spec_id {
            CustomSpecId::MainnetOsaka => Version::base(spec_id.into()),
            CustomSpecId::CustomOsaka => &CUSTOM_VERSION,
        }
    }

    fn evm_version<Cfg: EvmConfig>() -> &'static EvmVersion<CustomTypes> {
        if core::ptr::addr_eq(Cfg::VERSION, &CUSTOM_VERSION) {
            &CUSTOM_EVM_VERSION
        } else {
            const { &EvmVersion::<CustomTypes>::new_base::<Cfg>() }
        }
    }
}

fn run_base_osaka(
    interpreter: &mut evm2::interpreter::Interpreter<CustomTypes>,
    host: &mut Evm<CustomTypes>,
) -> InstrStop {
    interpreter.run::<BaseEvmConfig<{ SpecId::OSAKA as u8 }>>(host)
}

#[instruction]
fn custom() -> out {
    *out = Word::from(0xdead_u64);
}

#[derive(Debug)]
struct CustomTx {
    target: Address,
    code: Bytes,
}

impl Typed2718 for CustomTx {
    fn ty(&self) -> u8 {
        CUSTOM_TX_TYPE
    }
}

const fn as_custom_tx(tx: &CustomTx) -> Option<&CustomTx> {
    Some(tx)
}

fn handle_custom_tx(
    req: TxRequest<'_, CustomTx, Evm<CustomTypes>>,
) -> HandlerResult<evm2::TxResult> {
    const GAS_LIMIT: u64 = 100_000;

    let message = Message {
        gas_limit: GAS_LIMIT,
        destination: req.tx.target,
        code_address: req.tx.target,
        ..Message::default()
    };
    let result = req.host.execute_message(
        Default::default(),
        Bytecode::new_legacy(req.tx.code.clone()),
        message,
        false,
    );
    Ok(evm2::TxResult {
        status: result.stop.is_success(),
        gas_used: GAS_LIMIT - result.gas_remaining,
        stop: result.stop,
        output: result.output,
    })
}

fn custom_registry() -> TxRegistry<CustomTx, evm2::TxResult, Evm<CustomTypes>> {
    TxRegistry::new().with_handler(CUSTOM_TX_TYPE, as_custom_tx, handle_custom_tx)
}

fn main() {
    assert_eq!(SpecId::from(CustomSpecId::MainnetOsaka), SpecId::OSAKA);

    let custom_target = Address::from([0xcc; 20]);
    let code = Bytes::from_static(&[CUSTOM_OPCODE, op::PUSH1, 0x01, op::SSTORE, op::STOP]);
    let tx = CustomTx { target: custom_target, code };

    let mut evm = Evm::<CustomTypes>::new(
        CustomSpecId::CustomOsaka,
        BlockEnv::default(),
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles,
    );
    assert_eq!(evm.spec_id(), SpecId::OSAKA);
    assert_eq!(evm.version().static_gas_table[CUSTOM_OPCODE], CUSTOM_OPCODE_GAS);

    let result = evm.transact(&tx).expect("custom transaction should execute");
    assert_eq!(result.stop, InstrStop::Stop);
    assert!(result.status);
    assert_eq!(
        evm.state().account_ref(custom_target).expect("custom target should exist").storage
            [&Word::from(1)]
            .current,
        Word::from(0xdead_u64),
    );

    let mut evm = Evm::<CustomTypes>::new(
        CustomSpecId::MainnetOsaka,
        BlockEnv::default(),
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles,
    );
    let result = evm.transact(&tx).expect("mainnet transaction should execute");
    assert_eq!(result.stop, InstrStop::OpcodeNotFound);
    assert!(!result.status);
}
