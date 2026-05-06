//! Defines a custom EVM config selector.
//!
//! The runtime `CustomSpecId` distinguishes base Osaka from custom Osaka, while the const generic
//! `BASE_SPEC_ID` always names the inherited base `SpecId`.

#![allow(clippy::missing_const_for_fn)]

use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, Bytes};
use evm2::{
    BaseEvmConfig, Evm, EvmConfig, EvmConfigSelector, EvmTypes, ExecutionConfig, SpecId, Version,
    VersionTables,
    bytecode::Bytecode,
    env::BlockEnv,
    evm::{InMemoryDB, precompile::NoPrecompiles},
    interpreter::{Host, InstrStop, Instruction, Message, Word, op},
    registry::{HandlerResult, TxRegistry, TxRequest},
    version::GasId,
};
use evm2_macros::instruction;

const CUSTOM_OPCODE: u8 = 0x0c;
const CUSTOM_OPCODE_GAS: u16 = 7;
const CUSTOM_OPCODE_DYNAMIC_GAS_ID: GasId = GasId::Custom0;
const CUSTOM_OPCODE_DYNAMIC_GAS: u32 = 3;
const CUSTOM_TX_TYPE: u8 = 0x7f;

// Runtime spec IDs

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

// EVM type family

#[derive(Debug)]
struct CustomTypes;

impl EvmTypes for CustomTypes {
    type ConfigSelector = CustomConfigSelector;
    type SpecId = CustomSpecId;
    type Tx = CustomTx;
    type Host = Evm<Self>;
    type Database = InMemoryDB;
    type Precompiles = NoPrecompiles;
}

// Version config

struct CustomConfig<const BASE_SPEC_ID: u8>(());

impl<const ID: u8> EvmConfig<CustomTypes> for CustomConfig<ID> {
    const BASE_SPEC_ID: SpecId = SpecId::try_from_u8(ID).unwrap();
    const VERSION_TABLES: &'static VersionTables<CustomTypes> = &custom_version_tables::<ID>();
}

const fn custom_version(base_spec_id: SpecId) -> Version {
    let mut version = *Version::base(base_spec_id);
    version.gas_params.set(CUSTOM_OPCODE_DYNAMIC_GAS_ID, CUSTOM_OPCODE_DYNAMIC_GAS);
    version
}

const fn custom_version_tables<const BASE_SPEC_ID: u8>() -> VersionTables<CustomTypes> {
    let mut version = VersionTables::<CustomTypes>::base::<CustomConfig<BASE_SPEC_ID>>();
    version.set_opcode(
        CUSTOM_OPCODE,
        CUSTOM_OPCODE_GAS,
        <custom<CustomTypes> as Instruction<CustomTypes>>::execute,
        true,
    );
    version
}

// Config selector

struct CustomConfigSelector(());

impl EvmConfigSelector<CustomTypes> for CustomConfigSelector {
    type Config<const BASE_SPEC_ID: u8> = CustomConfig<BASE_SPEC_ID>;

    fn execution_config(spec_id: CustomSpecId) -> ExecutionConfig<CustomTypes> {
        match spec_id {
            CustomSpecId::MainnetOsaka => {
                ExecutionConfig::for_config::<BaseEvmConfig<{ SpecId::OSAKA as u8 }>>()
            }
            CustomSpecId::CustomOsaka => {
                let base_spec_id = spec_id.into();
                ExecutionConfig::for_base_spec::<Self>(base_spec_id)
            }
        }
    }
}

// Custom instruction

#[instruction(dynamic_gas)]
fn custom(cx: _) -> Result<out> {
    cx.gas.spend(cx.state.gas_params().get(CUSTOM_OPCODE_DYNAMIC_GAS_ID).into())?;
    *out = Word::from(0xdead_u64);
}

// Custom transaction

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
        &Default::default(),
        Bytecode::new_legacy(req.tx.code.clone()),
        &message,
        false,
    );
    Ok(evm2::TxResult {
        status: result.stop.is_success(),
        gas_used: GAS_LIMIT - result.gas_remaining,
        stop: result.stop,
        output: result.output,
        ..Default::default()
    })
}

fn custom_registry() -> TxRegistry<CustomTx, evm2::TxResult, Evm<CustomTypes>> {
    TxRegistry::new().with_handler(CUSTOM_TX_TYPE, as_custom_tx, handle_custom_tx)
}

#[derive(Debug)]
struct Args {
    spec_id: CustomSpecId,
    version: Version,
}

impl Args {
    fn parse() -> Self {
        let spec_id = CustomSpecId::CustomOsaka;
        let mut version = custom_version(spec_id.into());
        version.memory_limit = 1 << 20;
        Self { spec_id, version }
    }
}

// End-to-end check

fn main() {
    assert_eq!(SpecId::from(CustomSpecId::MainnetOsaka), SpecId::OSAKA);
    let args = Args::parse();

    let custom_target = Address::from([0xcc; 20]);
    let code = Bytes::from_static(&[CUSTOM_OPCODE, op::PUSH1, 0x01, op::SSTORE, op::STOP]);
    let tx = CustomTx { target: custom_target, code };
    let expected_custom_gas = u64::from(CUSTOM_OPCODE_GAS)
        + u64::from(CUSTOM_OPCODE_DYNAMIC_GAS)
        + 3 // PUSH1
        + 2100 // Cold SSTORE load.
        + 20_000; // SSTORE zero to non-zero.

    let execution_config =
        CustomConfigSelector::execution_config(args.spec_id).with_version(args.version);

    let mut evm = Evm::<CustomTypes>::new_with_execution_config(
        execution_config,
        args.spec_id,
        BlockEnv::default(),
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles,
    );
    assert_eq!(evm.spec_id(), SpecId::OSAKA);
    assert_eq!(evm.version().memory_limit, args.version.memory_limit);
    assert_eq!(
        <CustomConfig<{ SpecId::OSAKA as u8 }> as EvmConfig<CustomTypes>>::VERSION_TABLES
            .static_gas(CUSTOM_OPCODE),
        CUSTOM_OPCODE_GAS,
    );
    assert_eq!(
        args.version.gas_params.get(CUSTOM_OPCODE_DYNAMIC_GAS_ID),
        CUSTOM_OPCODE_DYNAMIC_GAS,
    );

    let result = evm.transact(&tx).expect("custom transaction should execute");
    assert_eq!(result.stop, InstrStop::Stop);
    assert!(result.status);
    assert_eq!(result.gas_used, expected_custom_gas);

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
