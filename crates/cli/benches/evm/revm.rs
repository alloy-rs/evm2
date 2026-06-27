use crate::fixture::Suites;
use criterion::{BatchSize, BenchmarkGroup, black_box, measurement::WallTime};
use evm2::{SpecId, Version};
use evm2_cli::evm_bench::BenchCase;
use revm::{
    ExecuteEvm, MainBuilder, MainContext,
    context::{BlockEnv, CfgEnv, Context, TxEnv},
    database::{CacheDB, EmptyDB, InMemoryDB},
    primitives::{U256, hardfork::SpecId as RevmSpecId},
    statetest_types::{Test, TestSuite, TestUnit},
};
use std::{borrow::Cow, sync::Arc};

type BenchDB = CacheDB<Arc<InMemoryDB>>;
type BenchEvm = revm::MainnetEvm<revm::handler::MainnetContext<BenchDB>>;

#[derive(Clone)]
pub(crate) struct PreparedBench {
    name: Cow<'static, str>,
    cfg: CfgEnv,
    block: BlockEnv,
    db: Arc<InMemoryDB>,
    tx: TxEnv,
}

impl PreparedBench {
    pub(crate) fn load(bench: &BenchCase, suites: &Suites) -> Self {
        let spec = bench.transaction_spec().expect("transaction benchmark must have a spec");
        let suite = suites.get(bench.fixture_path);
        let (unit, test) = revm_case(suite.input(), &bench.name, spec);
        let tx = test.tx_env(&unit).expect("revm benchmark transaction must build");
        let (cfg, block) = envs(&unit, revm_spec_id(spec));
        let db = Arc::new(database(&unit));
        Self { name: bench.name.clone(), cfg, block, db, tx }
    }

    pub(crate) fn sanity_check(&self) {
        let mut runner = Runner::new(self);
        runner.run().unwrap_or_else(|err| {
            panic!("{} revm benchmark transaction must execute: {err:?}", self.name)
        });
    }

    pub(crate) fn bench(&self, group: &mut BenchmarkGroup<'_, WallTime>) {
        group.bench_function(format!("{}/revm", self.name), |b| {
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

fn revm_case(input: &str, name: &str, spec: SpecId) -> (TestUnit, Test) {
    let mut suite: TestSuite =
        serde_json::from_str(input).expect("fixture must parse as revm statetest");
    let mut unit = if let Some(unit) = suite.0.remove(name) {
        unit
    } else if suite.0.len() == 1 {
        suite.0.into_values().next().expect("fixture must contain a case")
    } else {
        panic!("fixture suite does not contain benchmark case {name}");
    };
    let tests = unit
        .post
        .iter_mut()
        .find(|(spec_name, _)| spec_name.to_spec_id() == revm_spec_id(spec))
        .map(|(_, tests)| tests)
        .expect("fixture suite must contain revm post test");
    let test = tests.remove(0);
    let gas_limit = &mut unit.transaction.gas_limit[test.indexes.gas];
    let cap = U256::from(Version::base(spec).tx_gas_limit_cap);
    if *gas_limit > cap {
        *gas_limit = cap;
    }
    (unit, test)
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
                .expect("revm benchmark storage must insert");
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
