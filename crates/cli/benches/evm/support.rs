use crate::{cases::BenchCase, fixture::Suites};
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::InMemoryDB,
};
use std::borrow::Cow;

type BenchEvm = Evm<BaseEvmTypes>;

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
        let mut runner = Runner::new(self);
        let _ = runner.run().unwrap_or_else(|err| {
            panic!("{} benchmark transaction must execute: {err:?}", self.name)
        });
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(self.name.as_ref(), |b| {
            b.iter_batched(
                || Runner::new(self),
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

struct Runner {
    evm: BenchEvm,
    tx: RecoveredTxEnvelope,
}

impl Runner {
    fn new(prepared: &PreparedBench) -> Self {
        Self {
            evm: new_evm(prepared.spec, prepared.block, prepared.db.clone()),
            tx: prepared.tx.clone(),
        }
    }

    fn run(&mut self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        self.evm.transact(&self.tx).map(evm2::ExecutedTx::commit)
    }
}

fn new_evm(spec: SpecId, block: BlockEnv, db: InMemoryDB) -> BenchEvm {
    Evm::new(spec, block, ethereum_tx_registry(spec), db, Precompiles::base(spec))
}
