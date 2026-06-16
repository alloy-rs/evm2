//! Simple JIT compiler example.

use alloy_primitives::{Bytes, hex};
use clap::Parser;
use evm2::{
    BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmConfigSelector, Precompiles, SpecId,
    bytecode::Bytecode,
    env::{BlockEnv, TxEnv},
    ethereum::ethereum_tx_registry,
    evm::EmptyDB,
    interpreter::{Interpreter, Message},
};
use evm2_jit::{EvmCompiler, evm2_api::EvmCompilerFn};
use eyre::Context;
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[arg(long, required_unless_present = "code_path")]
    code: Option<String>,
    #[arg(long, conflicts_with = "code")]
    code_path: Option<PathBuf>,
}

fn main() -> eyre::Result<()> {
    // Parse CLI arguments.
    let cli = Cli::parse();
    let code = match (cli.code, cli.code_path) {
        (Some(code), None) => code,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("Failed to read code from file: {path:?}"))?,
        _ => unreachable!(),
    };
    let bytecode = hex::decode(code.trim()).wrap_err("Failed to decode hex-encoded code")?;

    // Compile the code.
    let mut compiler = EvmCompiler::new_llvm(false)?;
    let f = unsafe { compiler.jit("test", bytecode.as_slice(), SpecId::CANCUN) }
        .wrap_err("Failed to JIT-compile code")?;
    let f = EvmCompilerFn::<BaseEvmTypes>::from_abi_compatible(f);

    // Set up runtime context and run the function.
    let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
        SpecId::CANCUN,
    );
    let tx_env = TxEnv::default();
    let message = Message { gas_limit: 1_000_000, ..Default::default() };
    let mut interpreter = Interpreter::<BaseEvmTypes>::new(
        Bytecode::new_legacy(Bytes::from(bytecode)),
        &tx_env,
        &message,
        false,
    );
    let mut host = Evm::<BaseEvmTypes>::new(
        SpecId::CANCUN,
        BlockEnv::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        EmptyDB::default(),
        Precompiles::base(SpecId::CANCUN),
    );
    interpreter.prepare_jit_run(&config, &mut host);
    let result = unsafe { f.call_with_interpreter(&mut interpreter, &mut host) };
    eprintln!("stop: {result:?}");
    eprintln!("output: 0x{}", hex::encode(interpreter.output()));

    Ok(())
}
