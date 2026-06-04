use alloy_primitives::{Address, B256, U256};

#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureError {
    #[error("invalid block range: from {from} is greater than to {to}")]
    InvalidRange { from: u64, to: u64 },
    #[error("invalid RPC URL: {0}")]
    InvalidRpcUrl(String),
    #[error("RPC transport request failed")]
    Transport(#[from] alloy_provider::transport::TransportError),
    #[error("failed to create Tokio runtime for capture")]
    Runtime(#[source] std::io::Error),
    #[error("failed to encode JSON")]
    EncodeJson(#[source] serde_json::Error),
    #[error(
        "block {block_number} trace transaction count mismatch: expected {expected}, prestate {prestate}, diff {diff}"
    )]
    TraceTransactionCountMismatch {
        block_number: u64,
        expected: usize,
        prestate: usize,
        diff: usize,
    },
    #[error("invalid trace result: {0}")]
    InvalidTraceResult(&'static str),
    #[error("invalid hex value {0}")]
    InvalidHex(String),
    #[error("invalid integer value {0}")]
    InvalidNumber(String),
    #[error("failed to decode raw block RLP")]
    DecodeRawBlock(#[source] alloy_rlp::Error),
    #[error("trailing bytes after raw block RLP")]
    TrailingRawBlockRlp,
    #[error("failed to recover transaction signer")]
    RecoverSigner(#[source] alloy_consensus::crypto::RecoveryError),
    #[error("capture contains too many distinct execution versions")]
    TooManyCapturedVersions,
    #[error("capture contains no execution versions")]
    EmptyCapturedVersions,
    #[error("capture contains no blocks")]
    EmptyCapture,
    #[error("capture spans multiple specs, which one EEST network cannot represent yet")]
    MultipleSpecs,
    #[error("capture uses unsupported spec id {0}")]
    UnsupportedSpec(u32),
    #[error("capture has conflicting bytecode for code hash {code_hash}")]
    CodeHashCollision { code_hash: B256 },
    #[error(
        "withdrawal balance for {address} exceeds traced balance: traced {traced_balance}, withdrawals {withdrawal_balance}"
    )]
    WithdrawalBalanceUnderflow { address: Address, traced_balance: U256, withdrawal_balance: U256 },
    #[error("block hash mismatch: expected {expected}, got {actual}")]
    BlockHashMismatch { expected: B256, actual: B256 },
    #[error("block number mismatch: expected {expected}, got {actual}")]
    BlockNumberMismatch { expected: u64, actual: u64 },
    #[error("parent hash mismatch: expected {expected}, got {actual}")]
    ParentHashMismatch { expected: B256, actual: B256 },
    #[error("transaction count mismatch: expected {expected}, got {actual}")]
    TransactionCountMismatch { expected: usize, actual: usize },
    #[error("failed to write capture output {path}")]
    WriteOutput {
        path: String,
        #[source]
        source: std::io::Error,
    },
}
