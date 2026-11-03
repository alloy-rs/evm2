use crate::fixture::Suites;
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::InMemoryDB,
};
use evm2_cli::evm_bench::BenchCase;
use evm2_superinstr::{SuperInstrBackend, SuperInstrConfig, SuperInstrRunner};
use std::borrow::Cow;

type BenchEvm = Evm<'static, BaseEvmTypes>;

#[derive(Clone, Debug)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    spec: SpecId,
    block: BlockEnv,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
    /// Shared by every iteration, so translation happens once, in [`Self::sanity_check`].
    backend: SuperInstrBackend,
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
            tx: case.tx(),
            backend: SuperInstrBackend::new(SuperInstrConfig::default()),
        }
    }

    /// Checks that the backend matches the interpreter, and translates every contract the
    /// transaction runs.
    pub(crate) fn sanity_check(&self) {
        let mut evm = new_evm(self.spec, self.block, self.db.clone());
        let interpreter =
            evm.transact(&self.tx).map(evm2::ExecutedTx::commit).unwrap_or_else(|err| {
                panic!("{} interpreter benchmark transaction must execute: {err:?}", self.name)
            });
        let superinstr = Runner::new(self).run().unwrap_or_else(|err| {
            panic!("{} superinstruction benchmark transaction must execute: {err:?}", self.name)
        });
        assert_eq!(
            interpreter, superinstr,
            "{} interpreter and superinstruction results differ",
            self.name
        );
        let stats = self.backend.stats();
        eprintln!(
            "superinstr {}: {} contracts translated in {:.1} us, {} bails",
            self.name,
            stats.translations,
            stats.translate_nanos as f64 / 1e3,
            stats.bails
        );
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/superinstr/run", self.name), |b| {
            b.iter_batched(
                || Runner::new(self),
                |mut runner| {
                    black_box(runner.run().unwrap_or_else(|err| {
                        panic!(
                            "{} superinstruction benchmark transaction must execute: {err:?}",
                            self.name
                        )
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
        let mut evm = new_evm(prepared.spec, prepared.block, prepared.db.clone());
        evm.set_interpreter_runner(SuperInstrRunner::new(prepared.backend.clone()));
        Self { evm, tx: prepared.tx.clone() }
    }

    fn run(&mut self) -> evm2::registry::HandlerResult<evm2::TxResult> {
        self.evm.transact(&self.tx).map(evm2::ExecutedTx::commit)
    }
}

fn new_evm(spec: SpecId, block: BlockEnv, db: InMemoryDB) -> BenchEvm {
    Evm::new(spec, block, ethereum_tx_registry(spec), db, Precompiles::base(spec))
}
