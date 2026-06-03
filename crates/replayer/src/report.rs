use std::time::Duration;

pub(crate) fn progress(completed: usize, total_blocks: usize, total_transactions: usize) {
    if completed == total_blocks || completed.is_multiple_of(25) {
        eprintln!("replayed {completed}/{total_blocks} blocks ({total_transactions} txs)");
    }
}

pub(crate) fn prepared(blocks: usize, transactions: usize, elapsed: Duration) {
    eprintln!("prepared {blocks} blocks ({transactions} txs) in {elapsed:.2?}");
}

pub(crate) fn prepare_progress(completed: usize, total_blocks: usize) {
    if completed.is_multiple_of(100) || completed == total_blocks {
        eprintln!("prepared {completed}/{total_blocks} blocks");
    }
}

pub(crate) fn execute_progress(completed: usize, total_blocks: usize) {
    if completed.is_multiple_of(25) || completed == total_blocks {
        eprintln!("executed {completed}/{total_blocks} blocks");
    }
}

pub(crate) fn stream_summary(
    total_blocks: usize,
    total_transactions: usize,
    label: &str,
    elapsed: Duration,
) {
    println!(
        "replayed {total_blocks} blocks ({total_transactions} txs) from {label} in {elapsed:.2?}"
    );
}

pub(crate) fn execution_summary(
    total_blocks: usize,
    total_transactions: usize,
    label: &str,
    elapsed: Duration,
) {
    println!(
        "executed {total_blocks} blocks ({total_transactions} txs) from {label} in {elapsed:.2?} (evm2-only)"
    );
}

pub(crate) fn gas(label: &str, total_gas: u128, elapsed: Duration) {
    let seconds = elapsed.as_secs_f64();
    let mgas_per_s = if seconds > 0.0 { total_gas as f64 / seconds / 1_000_000.0 } else { 0.0 };
    println!(
        "{label} gas used: {total_gas} ({:.2} Mgas) at {mgas_per_s:.2} Mgas/s",
        total_gas as f64 / 1_000_000.0
    );
}
