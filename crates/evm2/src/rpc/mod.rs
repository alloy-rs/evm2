//! RPC helpers: state and block overrides, call fee resolution, and transaction request
//! conversion.
//!
//! These cover the pieces an `eth_call`-style endpoint needs on top of the EVM: applying
//! `stateOverrides` and `blockOverrides`, resolving the fee fields of a `TransactionRequest`, and
//! turning the request into an executable [`RecoveredTxEnvelope`].
//!
//! [`RecoveredTxEnvelope`]: crate::ethereum::RecoveredTxEnvelope

mod fees;
pub use fees::{CallFees, CallFeesError};

mod overrides;
pub use overrides::{StateOverrideError, apply_block_overrides, apply_state_overrides};

mod transaction;
pub use transaction::{EthTxEnvError, TryIntoTxEnv, normalize_transaction_request};
