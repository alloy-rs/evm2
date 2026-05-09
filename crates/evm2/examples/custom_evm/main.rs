//! A custom EVM integration with a custom spec, opcode, transaction envelope, and registry.

#![allow(missing_docs, clippy::missing_const_for_fn)]

pub mod config;
pub mod opcode;
pub mod tx;

use crate::config::CustomConfigSelector;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, Bytes};
use config::{CustomSpecId, CustomTypes, custom_version};
use evm2::{
    Evm, EvmConfigSelector, ExecutionConfig, SpecId, Version,
    env::BlockEnv,
    evm::{InMemoryDB, precompile::NoPrecompiles},
    inspector::Inspector,
    interpreter::{InstrStop, Interpreter, Message, MessageResult, op},
    registry::HandlerResult,
};
use std::{cell::RefCell, rc::Rc};
use tx::{CustomEnvelope, ExecuteCodeTx, custom_registry};

fn main() -> HandlerResult<()> {
    custom_opcode_showcase()?;
    mainnet_fallback_showcase()?;
    inspector_showcase()?;
    Ok(())
}

fn custom_opcode_showcase() -> HandlerResult<()> {
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
    Ok(())
}

fn mainnet_fallback_showcase() -> HandlerResult<()> {
    let mut evm = mainnet_evm();
    let tx = custom_opcode_tx(Bytes::from_static(&[opcode::CUSTOM_OPCODE, op::STOP]));

    let result = evm.transact(&tx)?;

    println!(
        "mainnet fallback: expected status=false stop=OpcodeNotFound; got status={} stop={:?}",
        result.status, result.stop,
    );

    assert_eq!(result.stop, InstrStop::OpcodeNotFound);
    assert!(!result.status);
    Ok(())
}

fn inspector_showcase() -> HandlerResult<()> {
    let mut evm = custom_evm();
    let inspector_state = Rc::new(RefCell::new(InspectorState::default()));
    evm.set_inspector(ExampleInspector(Rc::clone(&inspector_state)));
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
    let inspector_state = inspector_state.borrow();
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

fn custom_evm() -> Evm<CustomTypes> {
    Evm::<CustomTypes>::new_with_execution_config(
        custom_execution_config(),
        CustomSpecId::CustomOsaka,
        BlockEnv::default(),
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles::default(),
    )
}

fn mainnet_evm() -> Evm<CustomTypes> {
    Evm::<CustomTypes>::new(
        CustomSpecId::MainnetOsaka,
        BlockEnv::default(),
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

struct ExampleInspector(Rc<RefCell<InspectorState>>);

impl Inspector<CustomTypes> for ExampleInspector {
    fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, CustomTypes>) {
        self.0.borrow_mut().initialized += 1;
    }

    fn step(&mut self, interp: &mut Interpreter<'_, CustomTypes>) {
        let mut state = self.0.borrow_mut();
        state.steps += 1;
        state.opcodes.push(interp.opcode());
    }

    fn step_end(&mut self, _interp: &mut Interpreter<'_, CustomTypes>) {
        self.0.borrow_mut().step_ends += 1;
    }

    fn log(&mut self, _log: &alloy_primitives::Log) {
        self.0.borrow_mut().logs += 1;
    }

    fn call(&mut self, _message: &mut Message) -> Option<MessageResult> {
        self.0.borrow_mut().calls += 1;
        None
    }
}
