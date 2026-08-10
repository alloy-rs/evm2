use crate::fixture::Suites;
use alloy_primitives::{B256, hex};
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, InterpreterRunner, Precompiles, SpecId,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::InMemoryDB,
    interpreter::{InstrStop, Interpreter},
};
use evm2_cli::evm_bench::BenchCase;
use evm2_jit_context::EvmCompilerFn;
use evm2_jit_llvm::EvmLlvmBackend;
use evm2_jit_runtime::{EvmCompiler, OptimizationLevel};
use std::{borrow::Cow, collections::HashMap, sync::Arc};

type BenchEvm = Evm<'static, BaseEvmTypes>;
type LlvmCompiler = EvmCompiler<EvmLlvmBackend>;

const SKIP_COMPILE_JIT: &[&str] = &[
    "snailtracer",
    "seaport",
    "fiat_token",
    "uniswap_v2_pair",
    "univ2_router",
    "airdrop",
    "usdc_proxy",
];
const SKIP_ALL: &[&str] = &["seaport", "snailtracer"];

#[derive(Debug)]
pub(crate) struct Compiler {
    compiler: LlvmCompiler,
}

impl Compiler {
    pub(crate) fn new() -> Self {
        Self { compiler: EvmCompiler::new_llvm(false).expect("LLVM JIT compiler must initialize") }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    spec: SpecId,
    block: BlockEnv,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
    entry_bytecode: Option<alloy_primitives::Bytes>,
    functions: Arc<HashMap<B256, EvmCompilerFn>>,
}

impl PreparedBench {
    pub(crate) fn load(
        bench: &BenchCase,
        suites: &Suites,
        compiler: &mut Compiler,
    ) -> Option<Self> {
        if SKIP_ALL.contains(&bench.name.as_ref()) {
            return None;
        }

        let spec = bench.transaction_spec().expect("transaction benchmark must have a spec");
        let suite = suites.get(bench.fixture_path);
        let case = suite.case(&bench.name, spec);
        let accounts = case.compiled_accounts();
        if accounts.is_empty() {
            return None;
        }

        let mut functions = HashMap::new();
        for account in accounts {
            if functions.contains_key(&account.code_hash) {
                continue;
            }
            let name = format!("contract_{}", hex::encode(account.code_hash));
            let func = unsafe {
                compiler.compiler.jit(&name, account.bytecode.as_ref(), spec).unwrap_or_else(
                    |err| panic!("{} benchmark JIT compilation failed: {err:?}", bench.name),
                )
            };
            functions.insert(account.code_hash, func);
            compiler.compiler.clear_ir().expect("benchmark JIT compiler IR must clear");
        }

        Some(Self {
            name: bench.name.clone(),
            spec,
            block: case.block(),
            db: case.state(),
            tx: case.tx(spec),
            entry_bytecode: case.entry_bytecode(),
            functions: Arc::new(functions),
        })
    }

    pub(crate) fn sanity_check(&self) {
        let interpreter = self.run_interpreter().unwrap_or_else(|err| {
            panic!("{} interpreter benchmark transaction must execute: {err:?}", self.name)
        });
        let jit = self.run_jit().unwrap_or_else(|err| {
            panic!("{} JIT benchmark transaction must execute: {err:?}", self.name)
        });
        assert_eq!(
            interpreter.status, jit.status,
            "{} interpreter and JIT status differ",
            self.name
        );
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        if let Some(bytecode) = &self.entry_bytecode {
            group.bench_function(format!("{}/jit/translate", self.name), |b| {
                b.iter_batched_ref(
                    || new_compiler(OptimizationLevel::Default),
                    |compiler| {
                        compiler
                            .translate(self.name.as_ref(), bytecode.as_ref(), self.spec)
                            .unwrap();
                    },
                    BatchSize::PerIteration,
                )
            });

            if !SKIP_COMPILE_JIT.contains(&self.name.as_ref()) {
                group.bench_function(format!("{}/jit/compile", self.name), |b| {
                    b.iter_batched_ref(
                        || {
                            let mut compiler = new_compiler(OptimizationLevel::default());
                            let id = compiler
                                .translate(self.name.as_ref(), bytecode.as_ref(), self.spec)
                                .expect("benchmark translation must succeed");
                            (compiler, id)
                        },
                        |(compiler, id)| unsafe {
                            compiler
                                .jit_function(*id)
                                .expect("benchmark JIT compilation must succeed");
                        },
                        BatchSize::PerIteration,
                    )
                });
            }
        }

        group.bench_function(format!("{}/jit/run", self.name), |b| {
            b.iter_batched(
                || Runner::new(self),
                |mut runner| {
                    black_box(runner.run().unwrap_or_else(|err| {
                        panic!("{} JIT benchmark transaction must execute: {err:?}", self.name)
                    }))
                },
                BatchSize::SmallInput,
            );
        });
    }

    fn run_interpreter(&self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        let mut evm = new_evm(self.spec, self.block, self.db.clone());
        evm.transact(&self.tx).map(evm2::ExecutedTx::commit)
    }

    fn run_jit(&self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        let mut runner = Runner::new(self);
        runner.run()
    }
}

struct Runner {
    evm: BenchEvm,
    tx: RecoveredTxEnvelope,
}

impl Runner {
    fn new(prepared: &PreparedBench) -> Self {
        let mut evm = new_evm(prepared.spec, prepared.block, prepared.db.clone());
        evm.set_interpreter_runner(FixedJitRunner { functions: Arc::clone(&prepared.functions) });
        Self { evm, tx: prepared.tx.clone() }
    }

    fn run(&mut self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        self.evm.transact(&self.tx).map(evm2::ExecutedTx::commit)
    }
}

#[derive(Clone, Debug)]
struct FixedJitRunner {
    functions: Arc<HashMap<B256, EvmCompilerFn>>,
}

impl InterpreterRunner<BaseEvmTypes> for FixedJitRunner {
    fn run<'frame, 'host>(
        &self,
        config: &ExecutionConfig<BaseEvmTypes>,
        interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        host: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        let func = *self.functions.get(&interpreter.original_bytecode_hash())?;
        interpreter.prepare_run(config.base_spec_id(), config.version(), host);
        Some(unsafe { func.call_with_interpreter(interpreter) })
    }
}

fn new_evm(spec: SpecId, block: BlockEnv, db: InMemoryDB) -> BenchEvm {
    Evm::new(spec, block, ethereum_tx_registry(spec), db, Precompiles::base(spec))
}

fn new_compiler(opt_level: OptimizationLevel) -> LlvmCompiler {
    let mut compiler = EvmCompiler::new_llvm(false).expect("LLVM JIT compiler must initialize");
    compiler.set_opt_level(opt_level);
    compiler
}
