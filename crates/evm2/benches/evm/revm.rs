use crate::{cases::Bench, fixture::Suites};
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::SpecId;
use revm::{
    ExecuteEvm, MainBuilder, MainContext, SpecId as RevmSpecId,
    context::{BlockEnv, CfgEnv, Context, TxEnv},
    database::{CacheDB, EmptyDB, InMemoryDB},
    statetest_types::TestUnit,
};
use std::sync::Arc;

type BenchDB = CacheDB<Arc<InMemoryDB>>;
type BenchEvm = revm::MainnetEvm<revm::handler::MainnetContext<BenchDB>>;

#[derive(Clone)]
pub(crate) struct PreparedBench {
    name: &'static str,
    cfg: CfgEnv,
    block: BlockEnv,
    db: Arc<InMemoryDB>,
    tx: TxEnv,
}

impl PreparedBench {
    pub(crate) fn load(bench: &Bench, suites: &Suites) -> Self {
        let suite = suites.get(bench.fixture_path);
        let case = suite.case(bench.name).revm_case(bench.name, bench.spec);
        let tx =
            case.test.tx_env(&case.unit).expect("converted revm benchmark transaction must build");
        let (cfg, block) = envs(&case.unit, revm_spec_id(bench.spec));
        let db = Arc::new(database(&case.unit));
        Self { name: bench.name, cfg, block, db, tx }
    }

    pub(crate) fn sanity_check(&self) {
        let mut runner = Runner::new(self);
        runner.run().unwrap_or_else(|err| {
            panic!("{} revm benchmark transaction must execute: {err:?}", self.name)
        });
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/revm/transact", self.name), |b| {
            b.iter_batched(
                || Runner::new(self),
                |mut runner| {
                    black_box(runner.run().unwrap_or_else(|err| {
                        panic!("{} revm benchmark transaction must execute: {err:?}", self.name)
                    }))
                },
                BatchSize::SmallInput,
            );
        });
    }
}

struct Runner {
    evm: BenchEvm,
    tx: TxEnv,
}

impl Runner {
    fn new(prepared: &PreparedBench) -> Self {
        Self {
            evm: new_evm(
                prepared.cfg.clone(),
                prepared.block.clone(),
                CacheDB::new(Arc::clone(&prepared.db)),
            ),
            tx: prepared.tx.clone(),
        }
    }

    fn run(
        &mut self,
    ) -> Result<
        revm::context_interface::result::ExecResultAndState<
            revm::context_interface::result::ExecutionResult<
                revm::context_interface::result::HaltReason,
            >,
            revm::state::EvmState,
        >,
        revm::context_interface::result::EVMError<
            <BenchDB as revm::Database>::Error,
            revm::context_interface::result::InvalidTransaction,
        >,
    > {
        self.evm.transact(self.tx.clone())
    }
}

fn envs(unit: &TestUnit, spec: RevmSpecId) -> (CfgEnv, BlockEnv) {
    let mut cfg = CfgEnv::new();
    cfg.set_spec_and_mainnet_gas_params(spec);
    cfg = cfg.disable_tx_chain_id_check();
    cfg.chain_id = unit.env.current_chain_id.map(|chain_id| chain_id.to()).unwrap_or_default();
    let block = unit.block_env(&mut cfg);
    (cfg, block)
}

fn new_evm(cfg: CfgEnv, block: BlockEnv, db: BenchDB) -> BenchEvm {
    Context::mainnet().with_cfg(cfg).with_block(block).with_db(db).build_mainnet()
}

fn database(unit: &TestUnit) -> InMemoryDB {
    let mut db = InMemoryDB::new(EmptyDB::new());
    for (address, account) in &unit.pre {
        let mut info = revm::state::AccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code: Some(
                revm::state::Bytecode::new_raw_checked(account.code.clone())
                    .unwrap_or_else(|_| revm::state::Bytecode::new_legacy(account.code.clone())),
            ),
            ..Default::default()
        };
        db.insert_contract(&mut info);
        db.insert_account_info(*address, info);
        for (key, value) in &account.storage {
            db.insert_account_storage(*address, *key, *value)
                .expect("converted revm benchmark storage must insert");
        }
    }
    db
}

fn revm_spec_id(spec: SpecId) -> RevmSpecId {
    match spec {
        SpecId::FRONTIER => RevmSpecId::FRONTIER,
        SpecId::HOMESTEAD => RevmSpecId::HOMESTEAD,
        SpecId::TANGERINE => RevmSpecId::TANGERINE,
        SpecId::SPURIOUS_DRAGON => RevmSpecId::SPURIOUS_DRAGON,
        SpecId::BYZANTIUM => RevmSpecId::BYZANTIUM,
        SpecId::PETERSBURG => RevmSpecId::PETERSBURG,
        SpecId::ISTANBUL => RevmSpecId::ISTANBUL,
        SpecId::BERLIN => RevmSpecId::BERLIN,
        SpecId::LONDON => RevmSpecId::LONDON,
        SpecId::MERGE => RevmSpecId::MERGE,
        SpecId::SHANGHAI => RevmSpecId::SHANGHAI,
        SpecId::CANCUN => RevmSpecId::CANCUN,
        SpecId::PRAGUE => RevmSpecId::PRAGUE,
        SpecId::OSAKA => RevmSpecId::OSAKA,
        SpecId::AMSTERDAM => RevmSpecId::AMSTERDAM,
        _ => panic!("unsupported benchmark spec: {spec:?}"),
    }
}
