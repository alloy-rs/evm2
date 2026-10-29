use crate::fixture::Suites;
use alloy_primitives::Log;
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::{
    BaseEvmTypes, Evm, Inspector, Precompiles, SpecId,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::InMemoryDB,
    interpreter::{Interpreter, Message, MessageResult},
};
use evm2_cli::evm_bench::BenchCase;
use std::borrow::Cow;

type BenchEvm = Evm<'static, BaseEvmTypes>;

#[derive(Clone, Debug)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    spec: SpecId,
    block: BlockEnv,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
}

impl PreparedBench {
    pub(crate) fn load(bench: &BenchCase, suites: &Suites) -> Self {
        let spec = bench.transaction_spec().expect("transaction benchmark must have a spec");
        let suite = suites.get(bench.fixture_path);
        let case = suite.case(&bench.name, spec);
        Self {
            name: bench.name.clone(),
            spec,
            block: case.block(),
            db: case.state(),
            tx: case.tx(spec),
        }
    }

    pub(crate) fn sanity_check(&self) {
        let mut runner = Runner::new(self, None);
        let _ = runner.run().unwrap_or_else(|err| {
            panic!("{} benchmark transaction must execute: {err:?}", self.name)
        });
    }

    /// Checks that inspecting the transaction neither changes its result nor leaves hooks unused.
    fn sanity_check_inspected(&self, kind: InspectorKind) {
        let expected = Runner::new(self, None).run();
        let mut runner = Runner::new(self, Some(kind));
        let actual = runner.run();
        assert_eq!(actual, expected, "{} {} result must match the plain run", self.name, kind.id());
        runner.assert_observed(&self.name);
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        self.bench_with(group, self.name.to_string(), None);
    }

    /// Benchmarks the transaction with each [`InspectorKind`], after checking it.
    pub(crate) fn bench_inspected(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        for kind in [InspectorKind::Hooks, InspectorKind::Steps] {
            self.sanity_check_inspected(kind);
            self.bench_with(group, format!("{}/{}", self.name, kind.id()), Some(kind));
        }
    }

    fn bench_with(
        &self,
        group: &mut BenchmarkGroup<'_, WallTime>,
        id: String,
        kind: Option<InspectorKind>,
    ) {
        group.bench_function(id, |b| {
            b.iter_batched(
                || Runner::new(self, kind),
                |mut runner| {
                    black_box(runner.run().unwrap_or_else(|err| {
                        panic!("{} benchmark transaction must execute: {err:?}", self.name)
                    }))
                },
                BatchSize::SmallInput,
            );
        });
    }
}

/// Which inspector an inspected benchmark installs.
#[derive(Clone, Copy, Debug)]
enum InspectorKind {
    /// Cheap message and log hooks with the default no-op `step` and `step_end`, like an
    /// always-installed inspector stack that is not tracing. Any inspector makes the interpreter
    /// dispatch through its per-instruction hooks, so this still pays for the no-op calls.
    Hooks,
    /// Cheap `step` and `step_end` hooks, standing in for a tracer.
    Steps,
}

impl InspectorKind {
    const fn id(self) -> &'static str {
        match self {
            Self::Hooks => "inspect-hooks",
            Self::Steps => "inspect-steps",
        }
    }
}

/// Counts messages and logs.
#[derive(Default)]
struct HooksInspector {
    messages: u64,
    message_ends: u64,
    logs: u64,
}

impl Inspector<BaseEvmTypes> for HooksInspector {
    fn call(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.messages += 1;
        None
    }

    fn call_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Message<BaseEvmTypes>,
        _result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.message_ends += 1;
    }

    fn create(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.messages += 1;
        None
    }

    fn create_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Message<BaseEvmTypes>,
        _result: &mut MessageResult<BaseEvmTypes>,
    ) {
        self.message_ends += 1;
    }

    fn log(&mut self, _log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
        self.logs += 1;
    }
}

/// Counts instructions, and sums the stack depth and opcode values it reads on each.
#[derive(Default)]
struct StepsInspector {
    steps: u64,
    step_ends: u64,
    stack_len: u64,
    opcodes: u64,
}

impl Inspector<BaseEvmTypes> for StepsInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.steps += 1;
        self.opcodes += u64::from(interp.opcode());
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        self.step_ends += 1;
        self.stack_len += interp.stack().len() as u64;
    }
}

struct Runner {
    evm: BenchEvm,
    tx: RecoveredTxEnvelope,
    kind: Option<InspectorKind>,
}

impl Runner {
    fn new(prepared: &PreparedBench, kind: Option<InspectorKind>) -> Self {
        let mut evm = new_evm(prepared.spec, prepared.block, prepared.db.clone());
        match kind {
            Some(InspectorKind::Hooks) => evm.set_inspector(HooksInspector::default()),
            Some(InspectorKind::Steps) => evm.set_inspector(StepsInspector::default()),
            None => {}
        }
        Self { evm, tx: prepared.tx.clone(), kind }
    }

    fn run(&mut self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        self.evm.transact(&self.tx).map(evm2::ExecutedTx::commit)
    }

    /// Asserts that the installed inspector saw the transaction's messages or instructions.
    fn assert_observed(&mut self, name: &str) {
        match self.kind {
            Some(InspectorKind::Hooks) => {
                let hooks =
                    self.evm.clear_inspector_as::<HooksInspector>().expect("hooks inspector");
                assert!(hooks.messages > 0, "{name} hooks inspector must see a message");
                assert_eq!(hooks.messages, hooks.message_ends, "{name} messages must end");
            }
            Some(InspectorKind::Steps) => {
                let steps =
                    self.evm.clear_inspector_as::<StepsInspector>().expect("steps inspector");
                assert!(steps.steps > 0, "{name} steps inspector must see an instruction");
                assert_eq!(steps.steps, steps.step_ends, "{name} instructions must end");
            }
            None => {}
        }
    }
}

fn new_evm(spec: SpecId, block: BlockEnv, db: InMemoryDB) -> BenchEvm {
    Evm::new(spec, block, ethereum_tx_registry(spec), db, Precompiles::base(spec))
}
