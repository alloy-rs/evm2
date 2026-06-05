use crate::fuzzer::{
    case::{EvmCase, TxKindCase},
    normalize::{
        Outcome, OutcomeKind, TxReceipt, apply_account_changes, canonical_accounts, canonical_log,
        state_from_evm2_changes, state_from_revm,
    },
};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    ethereum::ethereum_tx_registry,
    evm::{AccountInfo as Evm2AccountInfo, InMemoryDB},
    interpreter::InstrStop,
};
use revm::{
    ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext,
    context::{CfgEnv, Context},
    context_interface::either::Either,
    database::{EmptyDB as RevmEmptyDB, InMemoryDB as RevmInMemoryDB, State as RevmState},
    primitives::hardfork::SpecId as RevmSpecId,
};

pub(crate) trait EvmBackend {
    fn name(&self) -> &'static str;

    fn run(&self, case: &EvmCase) -> Outcome;
}

pub(crate) struct Evm2Backend;

impl EvmBackend for Evm2Backend {
    fn name(&self) -> &'static str {
        "evm2"
    }

    fn run(&self, case: &EvmCase) -> Outcome {
        let mut database = evm2_db(case);
        let mut receipts = Vec::new();
        for tx in case.txs() {
            let mut evm = Evm::<BaseEvmTypes>::new(
                case.spec,
                case.block.evm2(),
                ethereum_tx_registry(case.spec),
                database.clone(),
                Precompiles::base(case.spec),
            );
            let result = evm
                .transact(&tx.evm2())
                .map(|executed| executed.detach())
                .map_err(|err| format!("{err:?}"));
            match result {
                Ok(result) => {
                    let output = if result.status || result.stop == InstrStop::Revert {
                        Some(result.output.to_vec())
                    } else {
                        None
                    };
                    database.commit_source(&result.state_changes);
                    receipts.push(TxReceipt {
                        kind: if result.status {
                            OutcomeKind::Success
                        } else {
                            OutcomeKind::RevertOrHalt
                        },
                        gas_used: Some(result.gas_used),
                        output,
                        logs: result.logs.iter().map(canonical_log).collect(),
                        state: state_from_evm2_changes(&result.state_changes),
                        error: None,
                    });
                }
                Err(err) => {
                    receipts.push(TxReceipt::error(err));
                    break;
                }
            }
        }
        Outcome::from_receipts(receipts)
    }
}

pub(crate) struct RevmBackend;

impl EvmBackend for RevmBackend {
    fn name(&self) -> &'static str {
        "revm"
    }

    fn run(&self, case: &EvmCase) -> Outcome {
        let mut cfg = CfgEnv::new();
        cfg.set_spec_and_mainnet_gas_params(revm_spec(case.spec));
        cfg = cfg.disable_tx_chain_id_check();
        let mut evm = Context::mainnet()
            .with_cfg(cfg)
            .with_block(case.block.revm())
            .with_db(RevmState::builder().with_database(revm_db(case)).build())
            .build_mainnet();

        let mut receipts = Vec::new();
        let mut accounts = canonical_accounts(case);
        for tx in case.txs() {
            let mut tx_env = tx.revm();
            if tx.kind == TxKindCase::Eip7702 {
                tx_env.authorization_list =
                    tx.eip7702_authorization_list().into_iter().map(Either::Left).collect();
            }
            match evm.transact(tx_env) {
                Ok(result) => {
                    let kind = if result.result.is_success() {
                        OutcomeKind::Success
                    } else {
                        OutcomeKind::RevertOrHalt
                    };
                    let state = result.state;
                    let canonical_state = state_from_revm(state.clone(), &accounts);
                    let receipt = TxReceipt {
                        kind,
                        gas_used: Some(result.result.tx_gas_used()),
                        output: result.result.output().map(|output| output.to_vec()),
                        logs: result.result.logs().iter().map(canonical_log).collect(),
                        state: canonical_state,
                        error: None,
                    };
                    evm.commit(state);
                    apply_account_changes(&mut accounts, &receipt.state);
                    receipts.push(receipt);
                }
                Err(err) => {
                    receipts.push(TxReceipt::error(format!("{err:?}")));
                    break;
                }
            }
        }
        Outcome::from_receipts(receipts)
    }
}

fn evm2_db(case: &EvmCase) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    for account in &case.accounts {
        db.insert_account_info(
            &account.address,
            Evm2AccountInfo::default()
                .with_balance(account.balance)
                .with_nonce(account.nonce)
                .with_code(Bytecode::new_legacy(account.code.clone())),
        );
        for (key, value) in &account.storage {
            db.insert_account_storage(&account.address, key, value);
        }
    }
    db
}

fn revm_db(case: &EvmCase) -> RevmInMemoryDB {
    let mut db = RevmInMemoryDB::new(RevmEmptyDB::new());
    for account in &case.accounts {
        let mut info = revm::state::AccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code: Some(revm::state::Bytecode::new_legacy(account.code.clone())),
            ..Default::default()
        };
        db.insert_contract(&mut info);
        db.insert_account_info(account.address, info);
        for (key, value) in &account.storage {
            if let Err(err) = db.insert_account_storage(account.address, *key, *value) {
                panic!("revm in-memory storage insertion failed: {err:?}");
            }
        }
    }
    db
}

const fn revm_spec(spec: SpecId) -> RevmSpecId {
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
        _ => RevmSpecId::CANCUN,
    }
}
