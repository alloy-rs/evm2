//! Simple JIT compiler example.

use clap::Parser;
use eyre::Context;
use evm2_jit::{
    EvmCompiler, SpecId as Evm2SpecId,
    interpreter::{
        context_interface::host::DummyHost,
        Interpreter,
        interpreter::{ExtBytecode, InputsImpl, SharedMemory},
    },
    primitives::hardfork::SpecId as RevmSpecId,
    revm_bytecode::Bytecode,
};
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
    let bytecode = evm2_jit::primitives::hex::decode(code.trim())
        .wrap_err("Failed to decode hex-encoded code")?;

    // Compile the code.
    let mut compiler = EvmCompiler::new_llvm(false)?;
    let f = unsafe { compiler.jit("test", &bytecode[..], Evm2SpecId::CANCUN) }
        .wrap_err("Failed to JIT-compile code")?;

    // Set up runtime context and run the function.
    let bytecode_obj = Bytecode::new_legacy(bytecode.into());
    let ext_bytecode = ExtBytecode::new(bytecode_obj);
    let input = InputsImpl::default();
    let memory = SharedMemory::new();
    let mut interpreter =
        Interpreter::new(memory, ext_bytecode, input, false, RevmSpecId::CANCUN, 1_000_000);
    let mut host = DummyHost::new(RevmSpecId::CANCUN);
    let result = unsafe { f.call_with_interpreter(&mut interpreter, &mut host) };
    eprintln!("{result:#?}");

    Ok(())
}
