use super::types::ForkSpec;
use alloy_primitives::U256;
use std::fmt;

/// Execution hooks for blockchain test replay.
pub trait Hook {
    /// Called before a selected test case starts.
    fn case_started(&mut self, _event: CaseStarted<'_>) {}

    /// Called before a block starts.
    fn block_started(&mut self, _event: BlockStarted) {}

    /// Called after a block completes.
    fn block_finished(&mut self, _event: BlockFinished) {}

    /// Called when block execution returns an error.
    fn block_failed(&mut self, _event: BlockFailed<'_>) {}

    /// Called before a transaction starts.
    fn transaction_started(&mut self, _event: TransactionStarted) {}

    /// Called after a transaction completes.
    fn transaction_finished(&mut self, _event: TransactionFinished) {}

    /// Called when transaction execution returns an unexpected error.
    fn transaction_failed(&mut self, _event: TransactionFailed<'_>) {}
}

/// Hook implementation that ignores every event.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopHook;

impl Hook for NoopHook {}

/// Test-case start event.
#[derive(Clone, Copy, Debug)]
pub struct CaseStarted<'a> {
    /// Test case name.
    pub name: &'a str,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Fork used by the case.
    pub network: ForkSpec,
}

/// Block start event.
#[derive(Clone, Copy, Debug)]
pub struct BlockStarted {
    /// Zero-based block index in the case.
    pub block_index: usize,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Block number, if the fixture provides a header.
    pub block_number: Option<U256>,
    /// Number of transactions in the block.
    pub total_transactions: usize,
}

/// Block finish event.
#[derive(Clone, Copy, Debug)]
pub struct BlockFinished {
    /// Zero-based block index in the case.
    pub block_index: usize,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Block number, if the fixture provides a header.
    pub block_number: Option<U256>,
}

/// Block failure event.
pub struct BlockFailed<'a> {
    /// Zero-based block index in the case.
    pub block_index: usize,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Block number, if the fixture provides a header.
    pub block_number: Option<U256>,
    /// Execution error.
    pub error: &'a dyn fmt::Display,
}

impl fmt::Debug for BlockFailed<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockFailed")
            .field("block_index", &self.block_index)
            .field("total_blocks", &self.total_blocks)
            .field("block_number", &self.block_number)
            .field("error", &self.error.to_string())
            .finish()
    }
}

/// Transaction start event.
#[derive(Clone, Copy, Debug)]
pub struct TransactionStarted {
    /// Zero-based block index in the case.
    pub block_index: usize,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Block number, if the fixture provides a header.
    pub block_number: Option<U256>,
    /// Zero-based transaction index in the block.
    pub transaction_index: usize,
    /// Number of transactions in the block.
    pub total_transactions: usize,
}

/// Transaction finish event.
#[derive(Clone, Copy, Debug)]
pub struct TransactionFinished {
    /// Zero-based block index in the case.
    pub block_index: usize,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Block number, if the fixture provides a header.
    pub block_number: Option<U256>,
    /// Zero-based transaction index in the block.
    pub transaction_index: usize,
    /// Number of transactions in the block.
    pub total_transactions: usize,
}

/// Transaction failure event.
pub struct TransactionFailed<'a> {
    /// Zero-based block index in the case.
    pub block_index: usize,
    /// Number of blocks in the case.
    pub total_blocks: usize,
    /// Block number, if the fixture provides a header.
    pub block_number: Option<U256>,
    /// Zero-based transaction index in the block.
    pub transaction_index: usize,
    /// Number of transactions in the block.
    pub total_transactions: usize,
    /// Execution error.
    pub error: &'a dyn fmt::Display,
}

impl fmt::Debug for TransactionFailed<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransactionFailed")
            .field("block_index", &self.block_index)
            .field("total_blocks", &self.total_blocks)
            .field("block_number", &self.block_number)
            .field("transaction_index", &self.transaction_index)
            .field("total_transactions", &self.total_transactions)
            .field("error", &self.error.to_string())
            .finish()
    }
}
