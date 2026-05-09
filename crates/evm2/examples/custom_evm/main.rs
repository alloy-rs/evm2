//! A custom EVM integration with a custom spec, opcode, transaction envelope, and registry.

#![allow(clippy::missing_const_for_fn)]

mod config;
mod opcode;
mod tx;

use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, Bytes};
use config::{CustomSpecId, CustomTypes, custom_version};
use evm2::{
    Evm, ExecutionConfig, SpecId, Version,
    env::BlockEnv,
    evm::{InMemoryDB, precompile::NoPrecompiles},
    interpreter::{InstrStop, op},
};
use tx::{CustomEnvelope, ExecuteCodeTx, custom_registry};

fn main() -> evm2::registry::HandlerResult<()> {
    let version = configured_custom_version();
    // Start from Osaka rules, then swap in the custom version tables and gas params.
    let execution_config =
        ExecutionConfig::<CustomTypes>::for_spec_and_version(CustomSpecId::CustomOsaka, version);

    let custom_target = Address::from([0xcc; 20]);
    // This bytecode only succeeds when the custom opcode has been installed.
    let execute_code_tx = CustomEnvelope::ExecuteCode(ExecuteCodeTx {
        target: custom_target,
        gas_limit: 100_000,
        code: Bytes::from_static(&[opcode::CUSTOM_OPCODE, op::PUSH1, 0x01, op::SSTORE, op::STOP]),
    });

    let mut evm = Evm::<CustomTypes>::new_with_execution_config(
        execution_config,
        CustomSpecId::CustomOsaka,
        BlockEnv::default(),
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles,
    );

    let execute_result = evm.transact(&execute_code_tx)?;
    let expected_custom_gas = u64::from(opcode::CUSTOM_OPCODE_GAS)
        + u64::from(opcode::CUSTOM_OPCODE_DYNAMIC_GAS)
        + 3
        + 2100
        + 20_000;
    assert_eq!(execute_result.stop, InstrStop::Stop);
    assert!(execute_result.status);
    assert_eq!(execute_result.gas_used, expected_custom_gas);

    // The same transaction still routes through the custom registry, but base Osaka
    // does not know the custom opcode.
    let mut mainnet_evm = Evm::<CustomTypes>::new(
        CustomSpecId::MainnetOsaka,
        BlockEnv::default(),
        custom_registry(),
        InMemoryDB::default(),
        NoPrecompiles,
    );
    assert_eq!(mainnet_evm.config_spec_id(), CustomSpecId::MainnetOsaka);
    let mainnet_result = mainnet_evm.transact(&execute_code_tx)?;
    assert_eq!(mainnet_result.stop, InstrStop::OpcodeNotFound);
    assert!(!mainnet_result.status);

    println!(
        "custom tx type=0x{:02x}: status={} gas_used={}",
        execute_code_tx.ty(),
        execute_result.status,
        execute_result.gas_used,
    );
    println!("mainnet fallback: status={} stop={:?}", mainnet_result.status, mainnet_result.stop,);

    Ok(())
}

fn configured_custom_version() -> Version {
    let mut version = custom_version(SpecId::OSAKA);
    version.memory_limit = 1 << 20;
    version
}
