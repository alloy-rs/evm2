mod block;
mod builder;
mod error;
mod export;
mod model;
mod overlay;
mod parse;
mod rpc;

pub(crate) use error::CaptureError;

use crate::{args::Capture, error::Result, ethereum};
use alloy_consensus::transaction::SignerRecoverable;
use alloy_primitives::Bytes;
use serde_json::Value;
use std::{fs::File, io::BufWriter, sync::Arc, time::Instant};
use tokio::{sync::Semaphore, task::JoinHandle};

const MAX_CONCURRENT_BLOCK_REQUESTS: usize = 8;

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
    let rpc = rpc::RpcEndpoint::parse(rpc_url)?;
    let mut builder = builder::CaptureBuilder::mainnet();
    let mut overlay = overlay::Overlay::default();
    let mut block_inputs = Vec::with_capacity((to - from + 1) as usize);
    let mut transaction_count = 0usize;

    builder.capture_block_hashes(&rpc, from).await?;
    let block_tasks = spawn_block_tasks(&rpc, from, to);

    for task in block_tasks {
        let block_started_at = Instant::now();
        let FetchedBlock { number, raw_block, consensus_block, pre_traces, diff_traces } =
            task.await.map_err(CaptureError::TaskJoin)??;
        if pre_traces.len() != consensus_block.body.transactions.len()
            || diff_traces.len() != consensus_block.body.transactions.len()
        {
            return Err(CaptureError::TraceTransactionCountMismatch {
                block_number: number,
                expected: consensus_block.body.transactions.len(),
                prestate: pre_traces.len(),
                diff: diff_traces.len(),
            });
        }

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
        let transactions = consensus_block
            .body
            .transactions
            .iter()
            .map(|tx| {
                let signer = tx.recover_signer().map_err(CaptureError::RecoverSigner)?;
                Ok(model::CapturedTransaction { signer })
            })
            .collect::<std::result::Result<Vec<_>, CaptureError>>()?;

        transaction_count += transactions.len();
        block_inputs.push(model::CapturedBlock {
            number: consensus_block.header.number,
            hash: consensus_block.header.hash_slow(),
            parent_hash: consensus_block.header.parent_hash,
            raw_block,
            transactions,
        });

        eprintln!(
            "captured block {number} ({} txs) in {:.2}s",
            consensus_block.body.transactions.len(),
            block_started_at.elapsed().as_secs_f64()
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
    raw_block: Bytes,
    consensus_block: block::MainnetBlock,
    pre_traces: Vec<Value>,
    diff_traces: Vec<Value>,
}

fn spawn_block_tasks(
    rpc: &rpc::RpcEndpoint,
    from: u64,
    to: u64,
) -> Vec<JoinHandle<std::result::Result<FetchedBlock, CaptureError>>> {
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_BLOCK_REQUESTS));
    (from..=to)
        .map(|number| {
            let rpc = rpc.clone();
            let permits = Arc::clone(&permits);
            tokio::spawn(async move {
                let _permit =
                    permits.acquire_owned().await.expect("capture semaphore is not closed");
                let block_id = rpc::hex_quantity(number);
                let raw_block = rpc.raw_block(&block_id).await?;
                let consensus_block = block::decode_consensus_block(&raw_block)?;
                let pre_traces = rpc.trace_block(&block_id, rpc::TraceMode::PreState).await?;
                let diff_traces = rpc.trace_block(&block_id, rpc::TraceMode::Diff).await?;
                Ok(FetchedBlock { number, raw_block, consensus_block, pre_traces, diff_traces })
            })
        })
        .collect()
}
