use crate::fuzzer::{
    case::{EvmCase, TxKindCase},
    normalize::{
        Outcome, OutcomeKind, TxReceipt, apply_account_changes, canonical_accounts, canonical_log,
        state_from_evm2_changes, state_from_revm,
    },
};
#[cfg(feature = "jit")]
use alloy_primitives::{B256, hex, keccak256};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    ethereum::ethereum_tx_registry,
    evm::{AccountInfo as Evm2AccountInfo, InMemoryDB},
    interpreter::InstrStop,
};
#[cfg(feature = "jit")]
use evm2::{ExecutionConfig, InterpreterRunner, interpreter::Interpreter};
#[cfg(feature = "jit")]
use evm2_jit_context::EvmCompilerFn;
#[cfg(feature = "jit")]
use evm2_jit_llvm::EvmLlvmBackend;
#[cfg(feature = "jit")]
use evm2_jit_runtime::{EvmCompiler, OptimizationLevel};
use revm::{
    ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext,
    context::{CfgEnv, Context},
    context_interface::either::Either,
    database::{EmptyDB as RevmEmptyDB, InMemoryDB as RevmInMemoryDB, State as RevmState},
    primitives::hardfork::SpecId as RevmSpecId,
};
#[cfg(feature = "jit")]
use std::{collections::HashMap, sync::Arc};

pub trait EvmBackend {
    fn name(&self) -> &'static str;

    fn run(&self, case: &EvmCase) -> Outcome;
}

#[derive(Clone, Copy, Debug)]
pub struct Evm2Backend;

impl EvmBackend for Evm2Backend {
    fn name(&self) -> &'static str {
        "evm2"
    }

    fn run(&self, case: &EvmCase) -> Outcome {
        run_evm2(case, |_| Ok(()))
    }
}

#[cfg(feature = "jit")]
#[derive(Clone, Copy, Debug)]
pub struct JitEvm2Backend;

#[cfg(feature = "jit")]
impl EvmBackend for JitEvm2Backend {
    fn name(&self) -> &'static str {
        "evm2-jit"
    }

    fn run(&self, case: &EvmCase) -> Outcome {
        let prepared = match PreparedJitCase::new(case) {
            Ok(prepared) => prepared,
            Err(err) => return Outcome::error(err),
        };
        let functions = Arc::clone(&prepared.functions);
        run_evm2(case, move |evm| {
            evm.set_interpreter_runner(FixedJitRunner { functions });
            Ok(())
        })
    }
}

fn run_evm2(
    case: &EvmCase,
    configure: impl FnOnce(&mut Evm<'_, BaseEvmTypes>) -> Result<(), String>,
) -> Outcome {
    let mut evm = Evm::<BaseEvmTypes>::new(
        case.spec,
        case.block.evm2(),
        ethereum_tx_registry(case.spec),
        evm2_db(case),
        Precompiles::base(case.spec),
    );
    if let Err(err) = configure(&mut evm) {
        return Outcome::error(err);
    }

    let mut receipts = Vec::new();
    for tx in case.txs() {
        let result = evm
            .transact(&tx.evm2())
            .map(|executed| executed.detach())
            .map_err(|err| format!("{err:?}"));
        // A resolved top-level transaction must clear all transaction-local state, or warm/touched
        // entries can leak into the next transaction and change EIP-2929 gas semantics.
        assert!(
            evm.state().transaction_state_is_empty(),
            "evm2 transact left transaction-local state behind"
        );
        match result {
            Ok(result) => {
                let tx_result = &result.result;
                let output = if tx_result.status || tx_result.stop == InstrStop::Revert {
                    Some(tx_result.output.to_vec())
                } else {
                    None
                };
                evm.commit_source(&result.pending_state);
                receipts.push(TxReceipt {
                    kind: if tx_result.status {
                        OutcomeKind::Success
                    } else {
                        OutcomeKind::RevertOrHalt
                    },
                    gas_used: Some(tx_result.tx_gas_used()),
                    output,
                    logs: tx_result.logs.iter().map(canonical_log).collect(),
                    state: state_from_evm2_changes(&result.pending_state),
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

#[cfg(feature = "jit")]
type LlvmCompiler = EvmCompiler<EvmLlvmBackend>;

#[cfg(feature = "jit")]
struct PreparedJitCase {
    _compiler: LlvmCompiler,
    functions: Arc<HashMap<B256, EvmCompilerFn>>,
}

#[cfg(feature = "jit")]
impl PreparedJitCase {
    fn new(case: &EvmCase) -> Result<Self, String> {
        let mut compiler = EvmCompiler::new_llvm(false).map_err(|err| format!("{err:?}"))?;
        compiler.set_opt_level(OptimizationLevel::None);

        let mut functions = HashMap::new();
        for account in &case.accounts {
            let bytecode = account.code.as_ref();
            if bytecode.is_empty() {
                continue;
            }

            let code_hash = keccak256(bytecode);
            if functions.contains_key(&code_hash) {
                continue;
            }

            let name = format!("fuzz_contract_{}", hex::encode(code_hash));
            let func = unsafe { compiler.jit(&name, bytecode, case.spec) }
                .map_err(|err| format!("JitCompilationFailed: {err:?}"))?;
            functions.insert(code_hash, func);
            compiler.clear_ir().map_err(|err| format!("{err:?}"))?;
        }

        Ok(Self { _compiler: compiler, functions: Arc::new(functions) })
    }
}

#[cfg(feature = "jit")]
#[derive(Clone, Debug)]
struct FixedJitRunner {
    functions: Arc<HashMap<B256, EvmCompilerFn>>,
}

#[cfg(feature = "jit")]
impl InterpreterRunner<BaseEvmTypes> for FixedJitRunner {
    fn run<'frame, 'host>(
        &self,
        config: &ExecutionConfig<BaseEvmTypes>,
        interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        host: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        let code = interpreter.original_bytecode();
        let func = *self.functions.get(&keccak256(&code))?;
        interpreter.prepare_run(config.base_spec_id(), config.version(), host);
        Some(unsafe { func.call_with_interpreter(interpreter) })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RevmBackend;

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
            let result = evm.transact(tx_env);
            let leftover = evm.finalize();
            // Revm transact already returns finalized state; a second finalize must not drain
            // anything, or transaction-local journal state survived the top-level transaction.
            assert!(
                leftover.is_empty(),
                "revm transact left transaction-local journal state: {leftover:#?}"
            );
            match result {
                Ok(result) => {
                    let kind = if result.result.is_success() {
                        OutcomeKind::Success
                    } else {
                        OutcomeKind::RevertOrHalt
                    };
                    let state = result.state;
                    let canonical_state = state_from_revm(state.clone(), case.spec, &accounts);
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

pub(super) fn evm2_db(case: &EvmCase) -> InMemoryDB {
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

pub(super) fn revm_db(case: &EvmCase) -> RevmInMemoryDB {
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

pub(super) const fn revm_spec(spec: SpecId) -> RevmSpecId {
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
