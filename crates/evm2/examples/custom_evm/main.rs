//! A custom EVM integration with a custom spec, opcode, transaction envelope, and registry.

#![allow(missing_docs, clippy::missing_const_for_fn)]

use crate::config::CustomConfigSelector;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, Bytes, U256};
use config::{CustomBlockEnvExt, CustomSpecId, CustomTypes, custom_version};
use evm2::{
    Evm, EvmConfigSelector, ExecutionConfig, SpecId, Version,
    env::BlockEnv,
    evm::{InMemoryDB, precompile::NoPrecompiles},
    inspector::Inspector,
    interpreter::{InstrStop, Interpreter, Message, MessageResult, op},
    registry::HandlerResult,
};
use tx::{CustomEnvelope, ExecuteCodeTx, custom_registry};

pub mod config;
pub mod opcode;
pub mod tx;

fn main() -> HandlerResult<()> {
    custom_opcode()?;
    l1_blocknumber_opcode()?;
    mainnet_fallback()?;
    inspector()?;
    Ok(())
}

fn custom_opcode() -> HandlerResult<()> {
    let mut evm = custom_evm();
    let tx = custom_opcode_tx(Bytes::from_static(&[
        opcode::CUSTOM_OPCODE,
        op::PUSH1,
        0x01,
        op::SSTORE,
        op::STOP,
    ]));

    let result = evm.transact(&tx)?;
    let expected_gas = u64::from(opcode::CUSTOM_OPCODE_GAS)
        + u64::from(opcode::CUSTOM_OPCODE_DYNAMIC_GAS)
        + 3
        + 2100
        + 20_000;

    println!(
        "custom opcode: tx_type=0x{:02x} expected status=true stop=Stop gas_used={expected_gas}; got status={} stop={:?} gas_used={}",
        tx.ty(),
        result.status,
        result.stop,
        result.gas_used,
    );

    assert_eq!(result.stop, InstrStop::Stop);
    assert!(result.status);
    assert_eq!(result.gas_used, expected_gas);
    assert!(result.ext.handled_custom_tx);
    Ok(())
}

fn l1_blocknumber_opcode() -> HandlerResult<()> {
    let mut evm = custom_evm();
    let tx = custom_opcode_tx(Bytes::from_static(&[
        opcode::L1_BLOCKNUMBER_OPCODE,
        op::PUSH0,
        op::MSTORE,
        op::PUSH1,
        32,
        op::PUSH0,
        op::RETURN,
    ]));

    let result = evm.transact(&tx)?;
    let expected = Bytes::copy_from_slice(&U256::from(CUSTOM_L1_BLOCK_NUMBER).to_be_bytes::<32>());

    println!(
        "l1 blocknumber opcode: expected status=true stop=Return output={expected:?}; got status={} stop={:?} output={:?}",
        result.status, result.stop, result.output,
    );

    assert_eq!(result.stop, InstrStop::Return);
    assert!(result.status);
    assert_eq!(result.output, expected);
    assert!(result.ext.handled_custom_tx);
    Ok(())
}

fn mainnet_fallback() -> HandlerResult<()> {
    let mut evm = mainnet_evm();
    let tx = custom_opcode_tx(Bytes::from_static(&[opcode::CUSTOM_OPCODE, op::STOP]));

    let result = evm.transact(&tx)?;

    println!(
        "mainnet fallback: expected status=false stop=InvalidOpcode; got status={} stop={:?}",
        result.status, result.stop,
    );

    assert_eq!(result.stop, InstrStop::InvalidOpcode);
    assert!(!result.status);
    Ok(())
}

fn inspector() -> HandlerResult<()> {
    let mut evm = custom_evm();
    evm.set_inspector(ExampleInspector::default());
    let tx = custom_opcode_tx(Bytes::from_static(&[
        op::PUSH1,
        0,
        op::PUSH1,
        0,
        op::LOG0,
        opcode::CUSTOM_OPCODE,
        op::STOP,
    ]));

    let result = evm.transact(&tx)?;
    let inspector = evm.clear_inspector().expect("inspector should be set");
    let inspector =
        inspector.downcast_ref::<ExampleInspector>().expect("inspector should have expected type");
    let inspector_state = &inspector.state;
    let expected_opcodes = [op::PUSH1, op::PUSH1, op::LOG0, opcode::CUSTOM_OPCODE, op::STOP];

    println!(
        "inspector: expected status=true initialized=1 steps=5 step_ends=5 logs=1 calls=0 opcodes={expected_opcodes:?}; got status={} initialized={} steps={} step_ends={} logs={} calls={} opcodes={:?}",
        result.status,
        inspector_state.initialized,
        inspector_state.steps,
        inspector_state.step_ends,
        inspector_state.logs,
        inspector_state.calls,
        inspector_state.opcodes,
    );

    assert!(result.status);
    assert_eq!(inspector_state.initialized, 1);
    assert_eq!(inspector_state.steps, expected_opcodes.len());
    assert_eq!(inspector_state.step_ends, expected_opcodes.len());
    assert_eq!(inspector_state.logs, 1);
    assert_eq!(inspector_state.calls, 0);
    assert_eq!(inspector_state.opcodes, expected_opcodes);
    Ok(())
}

const CUSTOM_L1_BLOCK_NUMBER: u64 = 42;
const MAINNET_L1_BLOCK_NUMBER: u64 = 1;

fn custom_evm() -> Evm<CustomTypes> {
    Evm::<CustomTypes>::new_with_execution_config(
        custom_execution_config(),
        CustomSpecId::CustomOsaka,
        BlockEnv {
            ext: CustomBlockEnvExt { l1_block_number: CUSTOM_L1_BLOCK_NUMBER },
            ..BlockEnv::default()
        },
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles::default(),
    )
}

fn mainnet_evm() -> Evm<CustomTypes> {
    Evm::<CustomTypes>::new(
        CustomSpecId::MainnetOsaka,
        BlockEnv {
            ext: CustomBlockEnvExt { l1_block_number: MAINNET_L1_BLOCK_NUMBER },
            ..BlockEnv::default()
        },
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles::default(),
    )
}

fn custom_execution_config() -> ExecutionConfig<CustomTypes> {
    CustomConfigSelector::execution_config(CustomSpecId::CustomOsaka)
        .with_version(configured_custom_version())
}

fn custom_opcode_tx(code: Bytes) -> CustomEnvelope {
    CustomEnvelope::ExecuteCode(ExecuteCodeTx {
        target: Address::from([0xcc; 20]),
        gas_limit: 100_000,
        code,
    })
}

pub fn configured_custom_version() -> Version {
    let mut version = custom_version(SpecId::OSAKA);
    version.memory_limit = 1 << 20;
    version
}

#[derive(Default)]
struct InspectorState {
    initialized: usize,
    steps: usize,
    step_ends: usize,
    logs: usize,
    calls: usize,
    opcodes: Vec<u8>,
}

#[derive(Default)]
struct ExampleInspector {
    state: InspectorState,
}

impl Inspector<CustomTypes> for ExampleInspector {
    fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, CustomTypes>) {
        self.state.initialized += 1;
    }

    fn step(&mut self, interp: &mut Interpreter<'_, CustomTypes>) {
        self.state.steps += 1;
        self.state.opcodes.push(interp.opcode());
    }

    fn step_end(&mut self, _interp: &mut Interpreter<'_, CustomTypes>) {
        self.state.step_ends += 1;
    }

    fn log(&mut self, _log: &alloy_primitives::Log) {
        self.state.logs += 1;
    }

    fn call(
        &mut self,
        _interp: &mut Interpreter<'_, CustomTypes>,
        _message: &mut Message<CustomTypes>,
    ) -> Option<MessageResult<CustomTypes>> {
        self.state.calls += 1;
        None
    }
}
