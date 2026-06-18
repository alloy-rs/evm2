mod builder;
mod error;
mod export;
mod model;
mod overlay;
mod parse;
mod rpc;

pub(crate) use error::CaptureError;

use crate::{args::Capture, error::Result, ethereum};
use alloy_consensus::{
    Block as ConsensusBlock, EthereumTxEnvelope, TxEip4844, transaction::SignerRecoverable,
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, value::RawValue};
use std::{
    fs::File,
    io::BufWriter,
    time::{Duration, Instant},
};

type MainnetBlock = ConsensusBlock<EthereumTxEnvelope<TxEip4844>>;

pub(crate) struct CaptureSummary {
    pub(crate) blocks: usize,
    pub(crate) transactions: usize,
    pub(crate) base_accounts: usize,
    pub(crate) base_storage_slots: usize,
    pub(crate) elapsed_sec: f64,
}

pub(crate) fn run(command: Capture) -> Result<()> {
    let from = *command.range.start();
    let to = *command.range.end();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| crate::error::Error::Capture { source: CaptureError::Runtime(source) })?;
    let summary = runtime
        .block_on(capture(&command.rpc, from, to, &command))
        .map_err(|source| crate::error::Error::Capture { source })?;
    println!(
        "captured EEST {}: {} blocks, {} txs, {} base accounts, {} base storage slots in {:.2}s",
        command.output.display(),
        summary.blocks,
        summary.transactions,
        summary.base_accounts,
        summary.base_storage_slots,
        summary.elapsed_sec
    );
    Ok(())
}

async fn capture(
    rpc_url: &str,
    from: u64,
    to: u64,
    command: &Capture,
) -> std::result::Result<CaptureSummary, CaptureError> {
    if from > to {
        return Err(CaptureError::InvalidRange { from, to });
    }

    let started_at = Instant::now();
    let rpc = rpc::RpcEndpoint::parse(
        rpc_url,
        command.max_concurrent_requests.get(),
        command.rpc_retries,
    )?;
    let mut builder = builder::CaptureBuilder::mainnet();
    let mut overlay = overlay::Overlay::default();
    let mut block_inputs = Vec::with_capacity((to - from + 1) as usize);
    let mut transaction_count = 0usize;

    builder.capture_block_hashes(&rpc, from).await?;
    let mut blocks = stream::iter(from..=to)
        .map(|number| fetch_block(&rpc, number))
        .buffered(rpc.max_concurrent_requests());

    while let Some(block) = blocks.next().await {
        let block_started_at = Instant::now();
        let PreparedBlock {
            number,
            consensus_block,
            pre_traces,
            diff_traces,
            transactions,
            elapsed,
        } = block?;

        for (tx_index, ((pre_trace, diff_trace), tx)) in pre_traces
            .iter()
            .zip(diff_traces.iter())
            .zip(consensus_block.body.transactions.iter())
            .enumerate()
        {
            let pre = parse::trace_result(pre_trace)?;
            let diff = parse::trace_result(diff_trace)?;
            let post = diff.get("post").unwrap_or(&Value::Null);

            builder.capture_base_requirements(number, tx_index, pre, &overlay)?;
            overlay.apply_post(post);
            overlay.apply_authorization_writes(tx, builder.chain_id());
        }
        overlay.apply_withdrawals(&consensus_block);

        let spec = ethereum::mainnet_spec_for_header(&consensus_block.header);
        let _version_index = builder.version_index(spec)?;
        let block_transaction_count = transactions.len();
        transaction_count += block_transaction_count;
        block_inputs.push(model::CapturedBlock { block: consensus_block, transactions });

        eprintln!(
            "captured block {number} ({} txs) in {:.2}s",
            block_transaction_count,
            (elapsed + block_started_at.elapsed()).as_secs_f64()
        );
    }

    let capture = builder.finish(block_inputs);
    let base_accounts = capture.pre_state.accounts.len();
    let base_storage_slots =
        capture.pre_state.accounts.iter().map(|account| account.storage.len()).sum();
    let suite = export::suite(&capture)?;
    let file = File::create(&command.output).map_err(|source| CaptureError::WriteOutput {
        path: command.output.display().to_string(),
        source,
    })?;
    serde_json::to_writer(BufWriter::new(file), &suite).map_err(CaptureError::EncodeJson)?;

    Ok(CaptureSummary {
        blocks: match &capture.input {
            model::CapturedInput::Block(_) => 1,
            model::CapturedInput::Blocks(blocks) => blocks.blocks.len(),
        },
        transactions: transaction_count,
        base_accounts,
        base_storage_slots,
        elapsed_sec: started_at.elapsed().as_secs_f64(),
    })
}

struct FetchedBlock {
    number: u64,
    consensus_block: MainnetBlock,
    pre_traces: Box<RawValue>,
    diff_traces: Box<RawValue>,
}

struct PreparedBlock {
    number: u64,
    consensus_block: MainnetBlock,
    pre_traces: Vec<Value>,
    diff_traces: Vec<Value>,
    transactions: Vec<model::CapturedTransaction>,
    elapsed: Duration,
}

async fn fetch_block(
    rpc: &rpc::RpcEndpoint,
    number: u64,
) -> std::result::Result<PreparedBlock, CaptureError> {
    let started_at = Instant::now();
    let (consensus_block, pre_traces, diff_traces) = tokio::try_join!(
        rpc.block(number),
        rpc.trace_block(number, rpc::TraceMode::PreState),
        rpc.trace_block(number, rpc::TraceMode::Diff),
    )?;
    let mut block =
        prepare_block(FetchedBlock { number, consensus_block, pre_traces, diff_traces }).await?;
    block.elapsed = started_at.elapsed();
    Ok(block)
}

async fn prepare_block(block: FetchedBlock) -> std::result::Result<PreparedBlock, CaptureError> {
    tokio::task::spawn_blocking(move || {
        let FetchedBlock { number, consensus_block, pre_traces, diff_traces } = block;
        let pre_traces: Vec<Value> =
            serde_json::from_str(pre_traces.get()).map_err(CaptureError::DecodeTrace)?;
        let diff_traces: Vec<Value> =
            serde_json::from_str(diff_traces.get()).map_err(CaptureError::DecodeTrace)?;
        let expected = consensus_block.body.transactions.len();
        if pre_traces.len() != expected || diff_traces.len() != expected {
            return Err(CaptureError::TraceTransactionCountMismatch {
                block_number: number,
                expected,
                prestate: pre_traces.len(),
                diff: diff_traces.len(),
            });
        }

        let transactions = consensus_block
            .body
            .transactions
            .iter()
            .map(|tx| {
                let signer = tx.recover_signer().map_err(CaptureError::RecoverSigner)?;
                Ok(model::CapturedTransaction { signer })
            })
            .collect::<std::result::Result<Vec<_>, CaptureError>>()?;

        Ok(PreparedBlock {
            number,
            consensus_block,
            pre_traces,
            diff_traces,
            transactions,
            elapsed: Duration::default(),
        })
    })
    .await
    .map_err(CaptureError::JoinBlockPreparation)?
}
