use crate::fuzzer::{
    case::{EvmCase, FuzzTxKind},
    normalize::{
        CanonicalAccount, CanonicalState, FuzzOutcomeKind, Outcome, TxReceipt,
        apply_account_changes, canonical_accounts, canonical_log, state_from_revm,
    },
};
use alloy_primitives::{Address, map::HashMap};
use evm2::{
    BaseEvmConfigSelector, BaseEvmTypes, Evm, EvmConfigSelector, Precompiles, SpecId,
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

    fn run(&mut self, case: &EvmCase) -> Outcome;
}

#[derive(Default)]
pub(crate) struct Evm2Backend {
    evm: Option<Evm<'static, BaseEvmTypes>>,
}

impl EvmBackend for Evm2Backend {
    fn name(&self) -> &'static str {
        "evm2"
    }

    fn run(&mut self, case: &EvmCase) -> Outcome {
        let evm = if let Some(evm) = &mut self.evm {
            evm.set_block(case.block.evm2());
            if evm.spec_id() != case.spec {
                evm.set_execution_config(
                    BaseEvmConfigSelector::execution_config(case.spec),
                    case.spec,
                    ethereum_tx_registry(case.spec),
                    Precompiles::base(case.spec),
                );
            }
            evm.set_database(evm2_db(case));
            evm
        } else {
            self.evm.insert(Evm::new(
                case.spec,
                case.block.evm2(),
                ethereum_tx_registry(case.spec),
                evm2_db(case),
                Precompiles::base(case.spec),
            ))
        };
        let mut receipts = Vec::with_capacity(1 + case.extra_txs.len());
        for tx in case.txs() {
            let result = evm.transact(&tx.evm2());
            match result {
                Ok(result) => {
                    let mut state = CanonicalState::default();
                    let Ok(tx_result) = result.commit_with(&mut state);
                    let output = if tx_result.status || tx_result.stop == InstrStop::Revert {
                        Some(tx_result.output.to_vec())
                    } else {
                        None
                    };
                    receipts.push(TxReceipt {
                        kind: if tx_result.status {
                            FuzzOutcomeKind::Success
                        } else {
                            FuzzOutcomeKind::RevertOrHalt
                        },
                        gas_used: Some(tx_result.tx_gas_used()),
                        output,
                        logs: tx_result.logs.iter().map(canonical_log).collect(),
                        state,
                        error: None,
                    });
                }
                Err(err) => {
                    receipts.push(TxReceipt::error(err.into()));
                    break;
                }
            }
        }
        Outcome::from_receipts(receipts)
    }
}

type RevmExecutor = revm::MainnetEvm<revm::handler::MainnetContext<RevmState<RevmInMemoryDB>>>;

#[derive(Default)]
pub(crate) struct RevmBackend {
    evm: Option<RevmExecutor>,
    accounts: HashMap<Address, CanonicalAccount>,
}

impl EvmBackend for RevmBackend {
    fn name(&self) -> &'static str {
        "revm"
    }

    fn run(&mut self, case: &EvmCase) -> Outcome {
        let mut cfg = CfgEnv::new();
        cfg.set_spec_and_mainnet_gas_params(revm_spec(case.spec));
        cfg = cfg.disable_tx_chain_id_check();
        let context = Context::mainnet()
            .with_cfg(cfg)
            .with_block(case.block.revm())
            .with_db(RevmState::builder().with_database(revm_db(case)).build());
        let evm = if let Some(evm) = &mut self.evm {
            evm.ctx = context;
            evm.instruction = revm::handler::instructions::EthInstructions::new_mainnet_with_spec(
                revm_spec(case.spec),
            );
            evm
        } else {
            self.evm.insert(context.build_mainnet())
        };

        let mut receipts = Vec::with_capacity(1 + case.extra_txs.len());
        let accounts = &mut self.accounts;
        canonical_accounts(case, accounts);
        for tx in case.txs() {
            let mut tx_env = tx.revm();
            if tx.kind == FuzzTxKind::Eip7702 {
                tx_env.authorization_list =
                    tx.eip7702_authorization_list().into_iter().map(Either::Left).collect();
            }
            match evm.transact(tx_env) {
                Ok(result) => {
                    let kind = if result.result.is_success() {
                        FuzzOutcomeKind::Success
                    } else {
                        FuzzOutcomeKind::RevertOrHalt
                    };
                    let state = result.state;
                    let canonical_state = state_from_revm(&state, accounts);
                    let receipt = TxReceipt {
                        kind,
                        gas_used: Some(result.result.tx_gas_used()),
                        output: result.result.output().map(|output| output.to_vec()),
                        logs: result.result.logs().iter().map(canonical_log).collect(),
                        state: canonical_state,
                        error: None,
                    };
                    evm.commit(state);
                    apply_account_changes(accounts, &receipt.state);
                    receipts.push(receipt);
                }
                Err(err) => {
                    receipts.push(TxReceipt::error(err.into()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzer::{case::CaseGenerator, rng::Gen};

    #[test]
    fn reused_backends_match_fresh_executors() {
        let mut generator = CaseGenerator::default();
        let mut evm2 = Evm2Backend::default();
        let mut revm = RevmBackend::default();
        for seed in 0..512 {
            let case = generator.generate(&mut Gen::new(seed));
            let expected = RevmBackend::default().run(case);
            assert_eq!(revm.run(case), expected, "revm seed {seed}");
            assert_eq!(Evm2Backend::default().run(case), expected, "fresh evm2 seed {seed}");
            assert_eq!(evm2.run(case), expected, "reused evm2 seed {seed}");
        }
    }
}
