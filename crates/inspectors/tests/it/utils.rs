use alloy_primitives::{Address, Bytes};
use colorchoice::ColorChoice;
use evm2_inspectors::tracing::{TraceWriter, TraceWriterConfig, TracingInspector};

pub use crate::compat::*;

pub fn write_traces(tracer: &TracingInspector) -> String {
    write_traces_with(tracer, TraceWriterConfig::new().color_choice(ColorChoice::Never))
}

pub fn write_traces_with(tracer: &TracingInspector, config: TraceWriterConfig) -> String {
    let mut w = TraceWriter::with_config(Vec::<u8>::new(), config);
    w.write_arena(tracer.traces()).expect("failed to write traces to Vec<u8>");
    String::from_utf8(w.into_writer()).expect("trace writer wrote invalid UTF-8")
}

pub fn print_traces(tracer: &TracingInspector) {
    // Use `println!` so that the output is captured by the test runner.
    println!("{}", write_traces_with(tracer, TraceWriterConfig::new()));
}

pub fn deploy_contract(
    evm: &mut TestEvm,
    code: Bytes,
    deployer: Address,
    spec: SpecId,
) -> DeployResult {
    evm.ctx.spec = spec;
    let value = evm.ctx.tx.value;
    let result = evm
        .inspect_tx_commit(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Create,
            data: code,
            value,
            nonce: evm.ctx.tx.nonce,
            ..Default::default()
        })
        .expect("Expect to be executed");
    evm.ctx.tx.nonce += 1;
    DeployResult { result }
}

pub fn inspect_deploy_contract<I: InspectorSlot>(
    evm: &mut TestEvmWithInspector<I>,
    code: Bytes,
    deployer: Address,
    spec: SpecId,
) -> DeployResult {
    evm.ctx.spec = spec;
    let value = evm.ctx.tx.value;
    let result = evm
        .inspect_tx_commit(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Create,
            data: code,
            value,
            nonce: evm.ctx.tx.nonce,
            ..Default::default()
        })
        .expect("Expect to be executed");
    evm.ctx.tx.nonce += 1;
    DeployResult { result }
}
