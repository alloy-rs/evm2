use crate::{
    corpus,
    error::{Error, Result},
    execute,
    input::Plan,
    prepare::{self, PreparedBlock},
    report,
};
use std::{path::PathBuf, time::Instant};

pub(crate) fn run_streaming(plan: Plan) -> Result<()> {
    let started_at = Instant::now();
    let total_blocks = plan.files.len();
    let mut total_transactions = 0usize;
    let mut total_gas = 0u128;

    for (index, path) in plan.files.iter().enumerate() {
        let block = corpus::read_block(path)?;
        total_transactions += block.transactions.len();
        let block_number = block.block_number;
        let block_hash = block.block_hash;
        let block_context_error = |source| Error::BlockContext {
            index,
            block_number,
            block_hash,
            path: path.clone(),
            source: Box::new(source),
        };
        let prepared = prepare::prepare_block(&block).map_err(block_context_error)?;
        let execution = execute::execute_block(prepared).map_err(block_context_error)?;
        total_gas += u128::from(execution.gas_used);
        report::progress(index + 1, total_blocks, total_transactions);
    }

    let elapsed = started_at.elapsed();
    report::stream_summary(
        total_blocks,
        total_transactions,
        &plan.label.display().to_string(),
        elapsed,
    );
    report::gas("total", total_gas, elapsed);
    Ok(())
}

pub(crate) fn run_preloaded(plan: Plan) -> Result<()> {
    let (prepared, prep_elapsed, total_transactions) = preload_blocks(&plan.files)?;
    report::prepared(prepared.len(), total_transactions, prep_elapsed);

    let total_blocks = prepared.len();
    let started = Instant::now();
    let mut total_gas = 0u128;
    let mut completed = 0usize;
    for prepared_block in prepared {
        let block_number = prepared_block.block_number;
        let block_hash = prepared_block.block_hash;
        let execution = execute::execute_block(prepared_block).map_err(|error| {
            Error::ExecuteContext { completed, block_number, block_hash, source: Box::new(error) }
        })?;
        total_gas += u128::from(execution.gas_used);
        completed += 1;
        report::execute_progress(completed, total_blocks);
    }
    let elapsed = started.elapsed();
    report::execution_summary(
        total_blocks,
        total_transactions,
        &plan.label.display().to_string(),
        elapsed,
    );
    report::gas("evm2-only", total_gas, elapsed);
    Ok(())
}

fn preload_blocks(paths: &[PathBuf]) -> Result<(Vec<PreparedBlock>, std::time::Duration, usize)> {
    let started = Instant::now();
    let mut prepared = Vec::with_capacity(paths.len());
    let mut total_transactions = 0usize;
    for (index, path) in paths.iter().enumerate() {
        let block = corpus::read_block(path)?;
        total_transactions += block.transactions.len();
        let block_context_error = |source| Error::BlockContext {
            index,
            block_number: block.block_number,
            block_hash: block.block_hash,
            path: path.clone(),
            source: Box::new(source),
        };
        prepared.push(prepare::prepare_block(&block).map_err(block_context_error)?);
        report::prepare_progress(index + 1, paths.len());
    }
    Ok((prepared, started.elapsed(), total_transactions))
}
