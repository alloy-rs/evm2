//! `evm2 t8n`: a state transition tool with the inputs and outputs of go-ethereum's `evm t8n`.
//!
//! It applies the transactions of `txs.json` to the pre-state of `alloc.json` under the block
//! environment of `env.json` and writes `result.json` and the post-state `alloc.json`. Tracing
//! drives the inspectors of `evm2-inspectors`, so the traces can be diffed against go-ethereum's.

mod input;
mod output;
mod trace;

pub use input::{Alloc, AllocAccount, Env, Inputs, TxJson, parse_fork};
use output::ReceiptContext;
pub use output::{Output, TxTrace};
pub use trace::{TraceKind, Tracing};

use alloy_consensus::{
    Eip658Value, Receipt, ReceiptEnvelope, Transaction, TxType, Typed2718,
    proofs::{calculate_receipt_root, calculate_transaction_root, calculate_withdrawals_root},
};
use alloy_eips::eip7685::Requests;
use alloy_primitives::{Address, B256, Bloom, Bytes, Log, U256, address, b256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId, TxResultWithState,
    env::{BlockEnv, BlockEnvExt},
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{
        BEACON_ROOTS_ADDRESS, BUILDER_DEPOSIT_REQUEST_ADDRESS, BUILDER_EXIT_REQUEST_ADDRESS, Bal,
        BlockAccessIndex, CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS, InMemoryDB,
        PendingState, SystemTx, WITHDRAWAL_REQUEST_ADDRESS,
    },
    registry::HandlerError,
};
use evm2_eest::{
    blob_basefee, insert_account_with_storage, parse_bytecode, state_root_from_database,
    system_contract_has_code,
};
use evm2_inspectors::tracing::DebugInspector;
use std::path::PathBuf;

/// Transaction gas limit cap of EIP-7825, the cap of the execution dimension per transaction.
const MAX_TX_GAS: u64 = 1 << 24;

/// The deposit contract and the topic of its `DepositEvent`.
const DEPOSIT_CONTRACT: Address = address!("00000000219ab540356cBB839Cbe05303d7705Fa");
const DEPOSIT_TOPIC: B256 =
    b256!("649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5");

/// Hash of the block that contains the transactions, as in `evm t8n`.
const BLOCK_HASH: B256 = B256::new({
    let mut hash = [0; 32];
    hash[0] = 0x13;
    hash[1] = 0x37;
    hash
});

/// Failure while running `evm2 t8n`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An input or output file could not be read.
    #[error("failed to read {path}")]
    Read {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// An output file could not be written.
    #[error("failed to write {path}")]
    Write {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A file is not valid JSON of the expected shape.
    #[error("failed to decode JSON from {path}")]
    Json {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: serde_json::Error,
    },
    /// The fork name has no evm2 specification.
    #[error("unsupported fork `{0}`")]
    UnsupportedFork(String),
    /// The command line asks for something this tool does not do.
    #[error("{0}")]
    Unsupported(String),
    /// The tracer could not be created or produced no result.
    #[error("tracer failed: {0}")]
    Tracer(String),
    /// Block-level execution failed, as `evm t8n` fails.
    #[error("{0}")]
    Execution(String),
}

/// Command-line arguments, named like those of `evm t8n`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// File with the pre-state alloc.
    #[arg(long = "input.alloc", default_value = "alloc.json", value_name = "FILE")]
    pub input_alloc: PathBuf,
    /// File with the block environment.
    #[arg(long = "input.env", default_value = "env.json", value_name = "FILE")]
    pub input_env: PathBuf,
    /// File with the unsigned transactions; each carries a `secretKey`.
    #[arg(long = "input.txs", default_value = "txs.json", value_name = "FILE")]
    pub input_txs: PathBuf,
    /// Hardfork to execute under, by go-ethereum's name (for example `Cancun`).
    #[arg(long = "state.fork", default_value = "GrayGlacier", value_name = "NAME")]
    pub fork: String,
    /// Block reward in wei for the coinbase, or -1 to leave it alone. A reward of 0 still creates
    /// the coinbase account before Spurious Dragon and deletes it when empty after.
    #[arg(long = "state.reward", default_value_t = 0, allow_hyphen_values = true)]
    pub reward: i64,
    /// Directory the output files go to.
    #[arg(long = "output.basedir", default_value = ".", value_name = "DIR")]
    pub output_basedir: PathBuf,
    /// Where the result goes: a file name, `stdout` or `stderr`.
    #[arg(long = "output.result", default_value = "result.json", value_name = "FILE")]
    pub output_result: String,
    /// Where the post-state alloc goes: a file name, `stdout` or `stderr`.
    #[arg(long = "output.alloc", default_value = "alloc.json", value_name = "FILE")]
    pub output_alloc: String,
    /// File for the RLP of the executed transactions.
    #[arg(long = "output.body", value_name = "FILE")]
    pub output_body: Option<String>,
    /// Write an EIP-3155 JSON trace per transaction to `trace-<index>-<hash>.jsonl`.
    #[arg(long, conflicts_with_all = ["tracer", "structlog"])]
    pub trace: bool,
    /// Write the result of a geth tracer per transaction to `trace-<index>-<hash>.json`.
    ///
    /// A tracer name (`callTracer`, `prestateTracer`, `4byteTracer`, `flatCallTracer`,
    /// `muxTracer`, `noopTracer`, `erc7562Tracer`) or JavaScript tracer source.
    #[arg(long = "trace.tracer", value_name = "NAME_OR_SOURCE", conflicts_with = "structlog")]
    pub tracer: Option<String>,
    /// JSON configuration of the tracer.
    #[arg(long = "trace.jsonconfig", value_name = "JSON", requires = "tracer")]
    pub jsonconfig: Option<String>,
    /// Write the default struct logger frame, as `debug_traceTransaction` returns it, per
    /// transaction to `trace-<index>-<hash>.json`. Specific to evm2.
    #[arg(long = "trace.structlog")]
    pub structlog: bool,
    /// Include the memory in `--trace` and `--trace.structlog` output.
    #[arg(long = "trace.memory")]
    pub memory: bool,
    /// Leave the stack out of `--trace` and `--trace.structlog` output.
    #[arg(long = "trace.nostack")]
    pub nostack: bool,
    /// Include the return data in `--trace` and `--trace.structlog` output.
    #[arg(long = "trace.returndata")]
    pub returndata: bool,
}

/// Runs the command: reads the inputs, executes and writes the output files.
pub fn run(args: &Args) -> Result<(), Error> {
    let inputs =
        Inputs::read(&args.input_alloc, &args.input_env, &args.input_txs, &args.fork, args.reward)?;
    let tracing = Tracing::from_args(args)?;
    let output = execute(&inputs, &tracing)?;
    output.write(args)
}

/// Applies the transactions to the pre-state and collects the result, post-state and traces.
pub fn execute(inputs: &Inputs, tracing: &Tracing) -> Result<Output, Error> {
    let Inputs { alloc, env, txs, spec, reward } = inputs;
    let spec = *spec;
    let mut inspector = tracing.inspector()?;

    let mut post = InMemoryDB::default();
    for (address, account) in alloc {
        let mut info =
            evm2::evm::AccountInfo::default().with_code(parse_bytecode(account.code.clone()));
        info.nonce = u64::try_from(account.nonce)
            .map_err(|_| Error::Execution(format!("nonce of {address} overflows u64")))?;
        info.balance = account.balance;
        insert_account_with_storage(
            &mut post,
            *address,
            info,
            account.storage.iter().map(|(&key, &value)| (key, value)),
        );
    }
    let block_hashes = env.block_hashes.as_ref();
    for (number, hash) in block_hashes.into_iter().flatten() {
        post.insert_block_hash(&U256::from(*number), hash);
    }
    check_env(env, spec)?;
    if env.current_base_fee.is_none() && env.parent_base_fee.is_some() {
        return Err(Error::Unsupported(
            "set `currentBaseFee`: deriving it from the parent is not supported".into(),
        ));
    }
    if env.current_excess_blob_gas.is_none() && env.parent_excess_blob_gas.is_some() {
        return Err(Error::Unsupported(
            "set `currentExcessBlobGas`: deriving it from the parent is not supported".into(),
        ));
    }
    if let Some(index) = txs.iter().position(TxJson::lacks_recipient) {
        return Err(Error::Execution(format!(
            "failed unmarshalling txs-file: missing required field 'to' in transaction {index}"
        )));
    }
    let block = block_env(env, spec);
    let mut bal = Bal::new();
    let number = u64::try_from(env.current_number).unwrap_or(u64::MAX);

    if spec.enables(SpecId::CANCUN)
        && let Some(root) = env.parent_beacon_block_root
    {
        // Like the history call below, a failure of this call is ignored.
        let _ =
            system_call(spec, block, &mut post, &mut bal, 0, BEACON_ROOTS_ADDRESS, root.0.into());
    }
    if spec.enables(SpecId::PRAGUE)
        && let Some(hashes) = block_hashes
    {
        let hash = hashes.get(&number.wrapping_sub(1)).copied().unwrap_or_default();
        let _ = system_call(
            spec,
            block,
            &mut post,
            &mut bal,
            0,
            HISTORY_STORAGE_ADDRESS,
            hash.0.into(),
        );
    }

    let mut output = Output::default();
    let mut included = Vec::new();
    let mut receipts = Vec::new();
    let mut all_logs = Vec::new();
    let mut gas_used = 0u64;
    let (mut execution_used, mut state_used) = (0u64, 0u64);
    let mut blob_gas_used = 0u64;
    // go-ethereum numbers trace files by the transactions that reach its state transition, which
    // includes the ones it rejects there.
    let mut applied = 0;
    let block_gas_limit = u64::try_from(env.current_gas_limit).unwrap_or(u64::MAX);
    for (index, tx) in txs.iter().enumerate() {
        let signed = match tx.sign() {
            Ok(signed) => signed,
            Err(error) => {
                output.reject(index, error);
                continue;
            }
        };
        let tx_hash = *signed.envelope.tx_hash();
        let introduced = match signed.envelope.ty() {
            1 => SpecId::BERLIN,
            2 => SpecId::LONDON,
            3 => SpecId::CANCUN,
            4 => SpecId::PRAGUE,
            _ => SpecId::FRONTIER,
        };
        if !spec.enables(introduced) {
            output.reject(index, "transaction type not supported".to_string());
            continue;
        }
        if signed.envelope.is_eip4844() && block.blob_basefee.is_zero() {
            output.reject(index, "blob tx used but field env.ExcessBlobGas missing".to_string());
            continue;
        }
        let tx_blob_gas = signed.recovered.inner().blob_gas_used().unwrap_or_default();
        let max_blob_gas = max_blob_gas(spec);
        if blob_gas_used + tx_blob_gas > max_blob_gas {
            output.reject(
                index,
                format!(
                    "blob gas ({}) would exceed maximum allowance {max_blob_gas}",
                    blob_gas_used + tx_blob_gas
                ),
            );
            continue;
        }
        let gas_limit = signed.recovered.inner().gas_limit();
        let fits = if spec.enables(SpecId::AMSTERDAM) {
            // EIP-8037: the block has an execution and a state dimension, each of the block limit.
            gas_limit.min(MAX_TX_GAS) <= block_gas_limit - execution_used
                && gas_limit <= block_gas_limit - state_used
        } else {
            gas_limit <= block_gas_limit - gas_used
        };
        let trace_index = applied;
        applied += 1;
        if !fits {
            output.reject(index, "gas limit reached".to_string());
            output.traces.extend(tracing.empty(trace_index, tx_hash));
            continue;
        }

        let mut evm = new_evm(spec, block, &post);
        if let Some(inspector) = &mut inspector {
            inspector.fuse().map_err(|err| Error::Tracer(err.to_string()))?;
            // Log indices count on through the block, as in receipts.
            inspector.set_next_log_index(all_logs.len());
        }
        let result = match transact(&mut evm, &mut inspector, &signed.recovered) {
            Ok(result) => result,
            Err(error) => {
                output.reject(index, error.to_string());
                output.traces.extend(tracing.empty(trace_index, tx_hash));
                continue;
            }
        };
        // go-ethereum aborts when BLOCKHASH reads a block the environment does not list.
        for number in evm.overlay_db().cache.block_hashes.keys() {
            let listed = u64::try_from(*number).ok().is_some_and(|number| {
                block_hashes.is_some_and(|hashes| hashes.contains_key(&number))
            });
            if !listed {
                let reason = if block_hashes.is_some() {
                    "blockhash for that block not provided"
                } else {
                    "no blockhashes provided"
                };
                return Err(Error::Execution(format!("getHash({number}) invoked, {reason}")));
            }
        }
        let tx_index = receipts.len();
        if let Some(inspector) = &mut inspector {
            let trace = tracing.collect(
                inspector,
                (trace_index, tx_hash),
                &signed.recovered,
                &block,
                &result,
                evm.database_mut(),
            )?;
            output.traces.push(trace);
        }
        drop(evm);
        post.commit_source(&result.pending_state);
        bal.commit(BlockAccessIndex(tx_index as u64 + 1), result.pending_state);

        let result = result.result;
        blob_gas_used += tx_blob_gas;
        gas_used += result.tx_gas_used();
        execution_used += result.execution_gas_spent();
        state_used += result.state_gas_spent();
        let status = if spec < SpecId::BYZANTIUM {
            Eip658Value::PostState(state_root_from_database(&post))
        } else {
            Eip658Value::Eip658(result.status)
        };
        let contract_address = if signed.kind.is_create() {
            signed.caller.create(signed.nonce)
        } else {
            Address::ZERO
        };
        let receipt = ReceiptEnvelope::from_typed(
            TxType::try_from(signed.envelope.ty()).unwrap_or(TxType::Legacy),
            Receipt { status, cumulative_gas_used: gas_used, logs: result.logs.clone() }
                .with_bloom(),
        );
        let context = ReceiptContext {
            tx_index,
            contract_address,
            gas_used: result.tx_gas_used(),
            block_number: number,
            block_timestamp: u64::try_from(env.current_timestamp).unwrap_or(u64::MAX),
            first_log_index: all_logs.len(),
            blob_base_fee: block.blob_basefee,
            success: result.status,
        };
        output.add_receipt(&receipt, &signed.envelope, &context);
        receipts.push(receipt);
        all_logs.extend(result.logs);
        included.push(signed.envelope);
    }

    let post_index = receipts.len() as u64 + 1;
    if *reward >= 0 {
        reward_coinbase(&mut post, &mut bal, post_index, spec, env.current_coinbase, *reward);
    }
    if spec.enables(SpecId::SHANGHAI) {
        for withdrawal in env.withdrawals.iter().flatten() {
            credit_withdrawal(
                &mut post,
                &mut bal,
                post_index,
                withdrawal.address,
                withdrawal.amount_wei(),
            );
        }
    }
    let requests = if spec.enables(SpecId::PRAGUE) {
        let mut requests = Requests::default();
        let deposits = deposit_requests(&all_logs)?;
        if !deposits.is_empty() {
            requests.push_request_with_type(0, deposits);
        }
        let mut queues = vec![(WITHDRAWAL_REQUEST_ADDRESS, 1), (CONSOLIDATION_REQUEST_ADDRESS, 2)];
        if spec.enables(SpecId::AMSTERDAM) {
            queues
                .extend([(BUILDER_DEPOSIT_REQUEST_ADDRESS, 3), (BUILDER_EXIT_REQUEST_ADDRESS, 4)]);
        }
        for (address, request_type) in queues {
            if !system_contract_has_code(&post, address) {
                return Err(Error::Execution(format!(
                    "failed to process post-execution: empty system contract: no code at {address}"
                )));
            }
            let data =
                system_call(spec, block, &mut post, &mut bal, post_index, address, Bytes::new())?;
            if !data.is_empty() {
                requests.push_request_with_type(request_type, data);
            }
        }
        Some(requests)
    } else {
        None
    };

    output.finish(
        inputs,
        &post,
        blob_gas_used,
        if spec.enables(SpecId::AMSTERDAM) { execution_used.max(state_used) } else { gas_used },
        calculate_transaction_root(&included),
        calculate_receipt_root(&receipts),
        receipts.iter().map(|receipt| receipt.logs_bloom()).fold(
            Bloom::ZERO,
            |mut bloom, other| {
                bloom |= *other;
                bloom
            },
        ),
        keccak256(alloy_rlp::encode(&all_logs)),
        env.withdrawals.as_deref().map(calculate_withdrawals_root),
        requests,
        spec.enables(SpecId::AMSTERDAM).then(|| {
            let encoded = alloy_rlp::encode(bal.to_alloy_bal());
            (keccak256(&encoded), encoded.into())
        }),
    );
    output.body = alloy_rlp::encode(&included).into();
    Ok(output)
}

fn transact(
    evm: &mut Evm<'_, BaseEvmTypes>,
    inspector: &mut Option<DebugInspector>,
    tx: &RecoveredTxEnvelope,
) -> Result<TxResultWithState, HandlerError> {
    let executed = match inspector {
        Some(inspector) => evm.transact_with_inspector(tx, inspector)?,
        None => evm.transact(tx)?,
    };
    Ok(executed.detach())
}

fn new_evm(spec: SpecId, block: BlockEnv, db: &InMemoryDB) -> Evm<'static, BaseEvmTypes> {
    Evm::<BaseEvmTypes>::new(
        spec,
        block,
        ethereum_tx_registry(spec),
        db.clone(),
        Precompiles::base(spec),
    )
}

/// Runs a system call and applies its state changes, returning the call's output.
fn system_call(
    spec: SpecId,
    block: BlockEnv,
    post: &mut InMemoryDB,
    bal: &mut Bal,
    bal_index: u64,
    address: Address,
    data: Bytes,
) -> Result<Bytes, Error> {
    let mut evm = new_evm(spec, block, post);
    let executed = evm
        .system_call(SystemTx::new(address, data))
        .map_err(|err| Error::Execution(format!("system call to {address} failed: {err}")))?;
    let result = executed.detach();
    // A failed call changes no state but its reads still belong to the access list.
    if !result.result.status {
        bal.commit(BlockAccessIndex(bal_index), result.pending_state);
        return Err(Error::Execution(format!("system call to {address} failed")));
    }
    post.commit_source(&result.pending_state);
    bal.commit(BlockAccessIndex(bal_index), result.pending_state);
    Ok(result.result.output)
}

/// Pays the block reward, which touches the coinbase: an empty account is created before
/// Spurious Dragon and removed from then on.
fn reward_coinbase(
    post: &mut InMemoryDB,
    bal: &mut Bal,
    bal_index: u64,
    spec: SpecId,
    coinbase: Address,
    reward: i64,
) {
    let original = post.cache.accounts.get(&coinbase).cloned().flatten();
    let mut current = original.clone().unwrap_or_default();
    current.balance += U256::from(reward);
    let removed = spec.enables(SpecId::SPURIOUS_DRAGON) && current.is_empty();
    let mut pending = PendingState::default();
    pending.insert_account(coinbase, original, (!removed).then_some(current));
    post.commit_source(&pending);
    if reward != 0 {
        bal.commit(BlockAccessIndex(bal_index), pending);
    }
}

fn credit_withdrawal(
    post: &mut InMemoryDB,
    bal: &mut Bal,
    bal_index: u64,
    address: Address,
    amount: U256,
) {
    let original = post.cache.accounts.get(&address).cloned().flatten();
    let mut current = original.clone().unwrap_or_default();
    current.balance += amount;
    let mut pending = PendingState::default();
    pending.insert_account(address, original, (!current.is_empty()).then_some(current));
    post.commit_source(&pending);
    bal.commit(BlockAccessIndex(bal_index), pending);
}

/// The block's blob gas limit: six blobs from Cancun, nine from Prague and 21 from Amsterdam, as
/// go-ethereum's `evm t8n` configures them.
const fn max_blob_gas(spec: SpecId) -> u64 {
    const BLOB_GAS: u64 = 1 << 17;
    BLOB_GAS
        * match spec {
            spec if spec.enables(SpecId::AMSTERDAM) => 21,
            spec if spec.enables(SpecId::PRAGUE) => 9,
            _ => 6,
        }
}

/// The EIP-6110 deposit requests of the logs the deposit contract emitted, in order.
fn deposit_requests(logs: &[Log]) -> Result<Bytes, Error> {
    let mut requests = Vec::new();
    for log in logs {
        if log.address != DEPOSIT_CONTRACT || log.topics().first() != Some(&DEPOSIT_TOPIC) {
            continue;
        }
        let data = &log.data.data;
        if data.len() != 576 {
            return Err(Error::Execution(format!(
                "failed to process post-execution: failed to parse deposit logs: unable to parse deposit data: deposit wrong length: want 576, have {}",
                data.len()
            )));
        }
        // The ABI-encoded event holds, after five offsets and one length each: the 48 byte
        // public key, the withdrawal credentials, the amount, the 96 byte signature and the index.
        for range in [192..240, 288..320, 352..360, 416..512, 544..552] {
            requests.extend_from_slice(&data[range]);
        }
    }
    Ok(requests.into())
}

/// The checks `evm t8n` makes of the environment, in its order and wording.
fn check_env(env: &Env, spec: SpecId) -> Result<(), Error> {
    let config = |message: &str| Err(Error::Execution(message.into()));
    if spec.enables(SpecId::LONDON)
        && env.current_base_fee.is_none()
        && env.parent_base_fee.is_none()
    {
        return config("EIP-1559 config but missing 'parentBaseFee' in env section");
    }
    if spec.enables(SpecId::SHANGHAI) && env.withdrawals.is_none() {
        return config("Shanghai config but missing 'withdrawals' in env section");
    }
    if spec.enables(SpecId::MERGE) {
        if env.current_random.is_none() {
            return config("post-merge requires currentRandom to be defined in env");
        }
        if env.current_difficulty.is_some_and(|difficulty| !difficulty.is_zero()) {
            return config("post-merge difficulty must be zero (or omitted) in env");
        }
    } else if env.current_difficulty.is_none() {
        return Err(Error::Unsupported(
            "set `currentDifficulty`: deriving it from the parent is not supported".into(),
        ));
    }
    if spec.enables(SpecId::CANCUN) && env.parent_beacon_block_root.is_none() {
        return config("post-cancun env requires parentBeaconBlockRoot to be set");
    }
    Ok(())
}

fn block_env(env: &Env, spec: SpecId) -> BlockEnv {
    BlockEnvExt {
        number: env.current_number,
        beneficiary: env.current_coinbase,
        timestamp: env.current_timestamp,
        gas_limit: env.current_gas_limit,
        basefee: env.current_base_fee.unwrap_or_default(),
        difficulty: env.current_difficulty.unwrap_or_default(),
        prevrandao: env.current_random.unwrap_or_default(),
        blob_basefee: env
            .current_excess_blob_gas
            .map_or(U256::ZERO, |excess| U256::from(blob_basefee(excess, spec))),
        slot_num: env.slot_number.unwrap_or_default(),
        ext: (),
        _non_exhaustive: (),
    }
}
