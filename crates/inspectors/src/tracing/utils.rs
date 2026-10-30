//! Utility functions for evm2 related ops.
use crate::tracing::{TransactionContext, config::TraceStyle};
use alloc::{
    borrow::Cow,
    format,
    string::{String, ToString},
    vec::Vec,
};
use alloy_primitives::{Bytes, hex};
use alloy_rpc_types_eth::TransactionInfo;
use alloy_sol_types::{ContractError, GenericRevertReason};
use evm2::{
    AccountInfo, EvmTypes, EvmTypesHost, SpecId,
    constants::STACK_LIMIT,
    env::BlockEnv,
    evm::{DbResult, DynDatabase},
    interpreter::{
        InstrStop, Interpreter,
        instructions::{decode_pair, decode_single},
        opcode::{OpCode, op},
    },
};

/// Converts a non successful [`InstrStop`] to an error message.
///
/// Returns `None` if [`InstrStop::is_success`].
///
/// See also <https://github.com/ethereum/go-ethereum/blob/34d507215951fb3f4a5983b65e127577989a6db8/eth/tracers/native/call_flat.go#L39-L55>
pub(crate) fn fmt_error_msg(res: InstrStop, kind: TraceStyle) -> Option<String> {
    if res.is_success() {
        return None;
    }
    let msg = match res {
        InstrStop::Revert => {
            if kind.is_parity() { "Reverted" } else { "execution reverted" }.to_string()
        }
        InstrStop::OutOfGas | InstrStop::PrecompileOOG => {
            if kind.is_parity() { "Out of gas" } else { "out of gas" }.to_string()
        }
        InstrStop::OutOfFunds => if kind.is_parity() {
            "Insufficient balance for transfer"
        } else {
            "insufficient balance for transfer"
        }
        .to_string(),
        InstrStop::MemoryOOG => {
            if kind.is_parity() { "Out of gas" } else { "out of gas" }.to_string()
        }
        InstrStop::MemoryLimitOOG => {
            if kind.is_parity() { "Out of gas" } else { "out of gas: gas uint64 overflow" }
                .to_string()
        }
        InstrStop::InvalidOperandOOG => {
            if kind.is_parity() { "Out of gas" } else { "gas uint64 overflow" }.to_string()
        }
        InstrStop::InvalidFEOpcode => {
            if kind.is_parity() { "Bad instruction" } else { "invalid opcode: INVALID" }.to_string()
        }
        InstrStop::OpcodeNotFound
        | InstrStop::NotActivated
        | InstrStop::InvalidImmediateEncoding => {
            if kind.is_parity() { "Bad instruction" } else { "invalid opcode" }.to_string()
        }
        InstrStop::StackUnderflow => {
            if kind.is_parity() { "Stack underflow" } else { "stack underflow" }.to_string()
        }
        InstrStop::StackOverflow => {
            if kind.is_parity() { "Out of stack" } else { "stack overflow" }.to_string()
        }
        InstrStop::StateChangeDuringStaticCall | InstrStop::CallNotAllowedInsideStatic => {
            if kind.is_parity() { "Mutable Call In Static Context" } else { "write protection" }
                .to_string()
        }
        InstrStop::OutOfOffset => {
            if kind.is_parity() { "Out of bounds" } else { "return data out of bounds" }.to_string()
        }
        InstrStop::CreateContractSizeLimit => {
            if kind.is_parity() { "Out of gas" } else { "max code size exceeded" }.to_string()
        }
        InstrStop::CreateInitCodeSizeLimit => {
            if kind.is_parity() { "Out of gas" } else { "max initcode size exceeded" }.to_string()
        }
        InstrStop::CreateContractStartingWithEF => {
            if kind.is_parity() { "Invalid code" } else { "invalid code: must not begin with 0xef" }
                .to_string()
        }
        InstrStop::CreateCollision => if kind.is_parity() {
            "Contract address collision"
        } else {
            "contract address collision"
        }
        .to_string(),
        InstrStop::NonceOverflow => {
            if kind.is_parity() { "Nonce overflow" } else { "nonce uint64 overflow" }.to_string()
        }
        InstrStop::CallTooDeep => {
            if kind.is_parity() { "Max call depth exceeded" } else { "max call depth exceeded" }
                .to_string()
        }
        InstrStop::InvalidJump => {
            if kind.is_parity() { "Bad jump destination" } else { "invalid jump destination" }
                .to_string()
        }
        InstrStop::PrecompileError => {
            if kind.is_parity() { "Built-in failed" } else { "precompiled failed" }.to_string()
        }
        InstrStop::ReentrancySentryOOG => if kind.is_parity() {
            "Out of gas"
        } else {
            "out of gas: not enough gas for reentrancy sentry"
        }
        .to_string(),
        InstrStop::FatalExternalError => {
            if kind.is_parity() { "Fatal external error" } else { "fatal external error" }
                .to_string()
        }
        status => format!("{status:?}"),
    };

    Some(msg)
}

/// Returns whether geth raises the error of an opcode that halted with `stop` before it runs the
/// opcode.
///
/// Only these errors reach a tracer's `OnOpcode` hook: the stack checks and the gas charges. An
/// error raised by the opcode itself reaches the `OnFault` hook, which the struct logger does not
/// register.
///
/// Call this from `step_end`, where the interpreter still holds the opcode and the stack it
/// halted on.
pub(crate) fn fails_before_body<T: EvmTypes>(
    stop: InstrStop,
    interp: &Interpreter<'_, '_, T>,
) -> bool {
    halts_before_body(stop, OpCode::new_or_unknown(interp.opcode()), interp.stack().len())
}

/// Converts an [`InstrStop`] that halted an opcode to the error geth reports for it.
///
/// Returns `None` if [`InstrStop::is_success`]. Call this from `step_end`, where the interpreter
/// still holds the opcode and the stack it halted on.
///
/// See <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/core/vm/interpreter.go#L214-L287> and
/// <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/core/vm/errors.go>.
pub(crate) fn fmt_geth_halt<T: EvmTypes>(
    stop: InstrStop,
    interp: &Interpreter<'_, '_, T>,
) -> Option<String> {
    let immediate = interp.original_bytecode_slice().get(interp.pc() + 1).copied();
    geth_halt_msg(
        stop,
        OpCode::new_or_unknown(interp.opcode()),
        interp.stack().len(),
        immediate.unwrap_or_default(),
    )
}

fn halts_before_body(stop: InstrStop, op: OpCode, stack_len: usize) -> bool {
    match stop {
        // `DUPN`, `SWAPN` and `EXCHANGE` check the stack again against their immediate inside the
        // opcode.
        InstrStop::StackUnderflow => stack_len < min_stack(op),
        InstrStop::StackOverflow
        | InstrStop::CreateInitCodeSizeLimit
        | InstrStop::CallNotAllowedInsideStatic => true,
        // `LOG` and `TSTORE` check write protection inside the opcode, unlike these opcodes.
        InstrStop::StateChangeDuringStaticCall => is_gas_function_static_check(op),
        _ => stop.is_out_of_gas(),
    }
}

fn geth_halt_msg(stop: InstrStop, op: OpCode, stack_len: usize, immediate: u8) -> Option<String> {
    Some(match stop {
        InstrStop::StackUnderflow => {
            // The pre-opcode check reports the stack the opcode pops; `DUPN`, `SWAPN` and
            // `EXCHANGE` report the one their immediate needs.
            let required = if stack_len < min_stack(op) {
                min_stack(op)
            } else {
                required_stack(op, immediate).unwrap_or_else(|| min_stack(op))
            };
            format!("stack underflow ({stack_len} <=> {required})")
        }
        InstrStop::StackOverflow => {
            let max_stack =
                (STACK_LIMIT + usize::from(op.inputs())).saturating_sub(op.outputs().into());
            format!("stack limit reached {stack_len} ({max_stack})")
        }
        InstrStop::OpcodeNotFound | InstrStop::NotActivated => {
            format!("invalid opcode: {}", geth_op_name(op))
        }
        InstrStop::InvalidImmediateEncoding => {
            format!("invalid opcode: {} (operand: 0x{immediate:02x})", geth_op_name(op))
        }
        // The gas functions of these opcodes check for write protection, and geth wraps the errors
        // of gas functions in `out of gas`.
        InstrStop::CallNotAllowedInsideStatic => "out of gas: write protection".to_string(),
        InstrStop::StateChangeDuringStaticCall if is_gas_function_static_check(op) => {
            "out of gas: write protection".to_string()
        }
        stop => fmt_error_msg(stop, TraceStyle::Geth)?,
    })
}

/// Returns the error geth reports for a contract creation whose initcode returned but ran out of
/// gas paying for the code deposit.
///
/// See <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/core/vm/evm.go#L656-L710>.
pub(crate) const CODE_DEPOSIT_OUT_OF_GAS: &str = "contract creation code storage out of gas";

/// Returns geth's name for an opcode, including the opcodes evm2 does not define.
///
/// See <https://github.com/ethereum/go-ethereum/blob/a0cba163b1f17f6044df84502ff13036dffb5de1/core/vm/opcodes.go#L395-L453>.
pub(crate) fn geth_op_name(op: OpCode) -> Cow<'static, str> {
    if op.is_valid() {
        return Cow::Borrowed(op.as_str());
    }
    let name = match op.get() {
        0xd0 => "DATALOAD",
        0xd1 => "DATALOADN",
        0xd2 => "DATASIZE",
        0xd3 => "DATACOPY",
        0xe0 => "RJUMP",
        0xe1 => "RJUMPI",
        0xe2 => "RJUMPV",
        0xe3 => "CALLF",
        0xe4 => "RETF",
        0xe5 => "JUMPF",
        0xec => "EOFCREATE",
        0xee => "RETURNCONTRACT",
        0xf7 => "RETURNDATALOAD",
        0xf8 => "EXTCALL",
        0xf9 => "EXTDELEGATECALL",
        0xfb => "EXTSTATICCALL",
        byte => return Cow::Owned(format!("opcode {byte:#x} not defined")),
    };
    Cow::Borrowed(name)
}

/// Returns whether the gas function of an opcode checks for write protection, which geth reports
/// as a gas error before it runs the opcode.
const fn is_gas_function_static_check(op: OpCode) -> bool {
    matches!(op.get(), op::SSTORE | op::CREATE | op::CREATE2 | op::SELFDESTRUCT)
}

/// Returns the stack length geth requires before it runs an opcode.
fn min_stack(op: OpCode) -> usize {
    match op.get() {
        op::DUPN => 1,
        op::SWAPN | op::EXCHANGE => 2,
        _ => op.inputs().into(),
    }
}

/// Returns the stack length that `DUPN`, `SWAPN` and `EXCHANGE` need for their immediate.
fn required_stack(op: OpCode, immediate: u8) -> Option<usize> {
    match op.get() {
        op::DUPN => decode_single(immediate),
        op::SWAPN => decode_single(immediate).map(|n| n + 1),
        op::EXCHANGE => decode_pair(immediate).map(|(n, m)| n.max(m) + 1),
        _ => None,
    }
}

/// Formats memory data into a list of 32-byte hex-encoded chunks.
///
/// See: <https://github.com/ethereum/go-ethereum/blob/366d2169fbc0e0f803b68c042b77b6b480836dbc/eth/tracers/logger/logger.go#L450-L452>
pub(crate) fn convert_memory(data: &[u8]) -> Vec<String> {
    let mut memory = Vec::with_capacity(data.len().div_ceil(32));
    let (chunks, remainder) = data.as_chunks::<32>();
    for chunk in chunks {
        memory.push(hex::encode_prefixed(chunk));
    }
    if !remainder.is_empty() {
        let mut last_chunk = [0u8; 32];
        last_chunk[..remainder.len()].copy_from_slice(remainder);
        memory.push(hex::encode_prefixed(last_chunk));
    }
    memory
}

/// Get the gas used, accounting for refunds
#[inline]
pub(crate) fn gas_used(spec: SpecId, spent: u64, refunded: u64) -> u64 {
    let refund_quotient = if spec.enables(SpecId::LONDON) { 5 } else { 2 };
    spent - (refunded).min(spent / refund_quotient)
}

/// Loads the code for the given account from the account itself or the database
///
/// Returns None if the code hash is the KECCAK_EMPTY hash
#[inline]
pub(crate) fn load_account_code(
    db: &mut dyn DynDatabase,
    account: &AccountInfo,
) -> DbResult<Option<Bytes>> {
    if account.is_empty_code_hash() {
        return Ok(None);
    }
    if let Some(code) = &account.code {
        return Ok(Some(code.original_bytes()));
    }
    db.get_code_by_hash(&account.code_hash).map(|code| Some(code.original_bytes()))
}

/// Returns a non-empty revert reason if the output is a revert/error.
/// Follows Geth's UnpackRevert logic
/// <https://github.com/ethereum/go-ethereum/blob/4414e2833f92f437d0a68b53ed95ac5756a90a16/accounts/abi/abi.go#L278>.
#[inline]
pub(crate) fn maybe_revert_reason(output: &[u8]) -> Option<String> {
    match GenericRevertReason::decode(output)? {
        GenericRevertReason::ContractError(err) => {
            let reason = match err {
                // return the raw revert reason and don't use the revert's display message
                ContractError::Revert(revert) => revert.reason,
                // return as geth panic error string
                ContractError::Panic(panic) => panic.as_geth_str().to_string(),
                err => err.to_string(),
            };

            if reason.is_empty() || reason.trim_matches('\0').is_empty() {
                None
            } else {
                Some(reason)
            }
        }
        _ => None,
    }
}

/// Returns the [`TransactionInfo`] of a traced transaction.
pub(crate) fn transaction_info<T: EvmTypesHost>(
    tx_context: Option<&TransactionContext>,
    block_env: &BlockEnv<T>,
) -> TransactionInfo {
    #[allow(clippy::needless_update)]
    TransactionInfo {
        hash: tx_context.and_then(|c| c.tx_hash),
        index: tx_context.and_then(|c| c.tx_index.map(|i| i as u64)),
        block_hash: tx_context.and_then(|c| c.block_hash),
        block_number: Some(block_env.number.saturating_to()),
        base_fee: Some(block_env.basefee.saturating_to()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloy_sol_types::{GenericContractError, SolInterface};

    #[test]
    fn invalid_instruction_errors() {
        for (stop, geth, parity) in [
            (InstrStop::InvalidFEOpcode, "invalid opcode: INVALID", "Bad instruction"),
            (InstrStop::OpcodeNotFound, "invalid opcode", "Bad instruction"),
            (InstrStop::NotActivated, "invalid opcode", "Bad instruction"),
            (InstrStop::InvalidImmediateEncoding, "invalid opcode", "Bad instruction"),
        ] {
            assert_eq!(fmt_error_msg(stop, TraceStyle::Geth).as_deref(), Some(geth));
            assert_eq!(fmt_error_msg(stop, TraceStyle::Parity).as_deref(), Some(parity));
        }
    }

    #[test]
    fn geth_halts() {
        for (stop, op, stack_len, immediate, msg, before_body) in [
            (InstrStop::StackUnderflow, op::ADD, 0, 0, "stack underflow (0 <=> 2)", true),
            (InstrStop::StackUnderflow, op::DUPN, 0, 0, "stack underflow (0 <=> 1)", true),
            (InstrStop::StackUnderflow, op::DUPN, 1, 0, "stack underflow (1 <=> 145)", false),
            (InstrStop::StackUnderflow, op::SWAPN, 1, 0, "stack underflow (1 <=> 2)", true),
            (InstrStop::StackUnderflow, op::SWAPN, 2, 0, "stack underflow (2 <=> 146)", false),
            (InstrStop::StackUnderflow, op::EXCHANGE, 2, 0, "stack underflow (2 <=> 17)", false),
            (
                InstrStop::InvalidImmediateEncoding,
                op::DUPN,
                1,
                0x5b,
                "invalid opcode: DUPN (operand: 0x5b)",
                false,
            ),
            (InstrStop::StackOverflow, op::PUSH0, 1024, 0, "stack limit reached 1024 (1023)", true),
            (InstrStop::StackOverflow, op::DUP16, 1024, 0, "stack limit reached 1024 (1023)", true),
            (InstrStop::StackOverflow, op::SWAP1, 1025, 0, "stack limit reached 1025 (1024)", true),
            (InstrStop::OpcodeNotFound, 0xd0, 0, 0, "invalid opcode: DATALOAD", false),
            (
                InstrStop::OpcodeNotFound,
                0x0c,
                0,
                0,
                "invalid opcode: opcode 0xc not defined",
                false,
            ),
            (InstrStop::NotActivated, op::PUSH0, 0, 0, "invalid opcode: PUSH0", false),
            (InstrStop::InvalidFEOpcode, op::INVALID, 0, 0, "invalid opcode: INVALID", false),
            (
                InstrStop::StateChangeDuringStaticCall,
                op::SSTORE,
                2,
                0,
                "out of gas: write protection",
                true,
            ),
            (
                InstrStop::StateChangeDuringStaticCall,
                op::SELFDESTRUCT,
                1,
                0,
                "out of gas: write protection",
                true,
            ),
            (InstrStop::StateChangeDuringStaticCall, op::LOG0, 2, 0, "write protection", false),
            (InstrStop::StateChangeDuringStaticCall, op::TSTORE, 2, 0, "write protection", false),
            (
                InstrStop::CallNotAllowedInsideStatic,
                op::CALL,
                7,
                0,
                "out of gas: write protection",
                true,
            ),
            (
                InstrStop::CreateInitCodeSizeLimit,
                op::CREATE,
                0,
                0,
                "max initcode size exceeded",
                true,
            ),
            (InstrStop::OutOfGas, op::ADD, 2, 0, "out of gas", true),
            (InstrStop::MemoryOOG, op::MSTORE, 0, 0, "out of gas", true),
            (InstrStop::MemoryLimitOOG, op::MSTORE, 0, 0, "out of gas: gas uint64 overflow", true),
            (InstrStop::InvalidOperandOOG, op::MSTORE, 0, 0, "gas uint64 overflow", true),
            (InstrStop::InvalidJump, op::JUMP, 0, 0, "invalid jump destination", false),
            (InstrStop::OutOfOffset, op::RETURNDATACOPY, 0, 0, "return data out of bounds", false),
        ] {
            let op = OpCode::new_or_unknown(op);
            assert_eq!(
                geth_halt_msg(stop, op, stack_len, immediate).as_deref(),
                Some(msg),
                "{stop:?}"
            );
            assert_eq!(halts_before_body(stop, op, stack_len), before_body, "{stop:?}");
        }
    }

    #[test]
    fn decode_revert_reason() {
        let err = GenericContractError::Revert("my revert".into());
        let encoded = err.abi_encode();
        let reason = maybe_revert_reason(&encoded).unwrap();
        assert_eq!(reason, "my revert");
    }

    // <https://etherscan.io/tx/0x105707c8e3b3675a8424a7b0820b271cbe394eaf4d5065b03c273298e3a81314>
    #[test]
    fn decode_revert_reason_with_error() {
        let err = hex!(
            "08c379a000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000024556e697377617056323a20494e53554646494349454e545f494e5055545f414d4f554e5400000000000000000000000000000000000000000000000000000080"
        );
        let reason = maybe_revert_reason(&err[..]).unwrap();
        assert_eq!(reason, "UniswapV2: INSUFFICIENT_INPUT_AMOUNT");
    }

    #[test]
    fn decode_revert_reason_with_null_bytes() {
        let empty_err = GenericContractError::Revert("".into());
        let encoded = empty_err.abi_encode();
        assert!(maybe_revert_reason(&encoded).is_none());

        let null_bytes_err =
            GenericContractError::Revert(String::from_utf8(vec![0u8; 32]).unwrap().into());
        let encoded = null_bytes_err.abi_encode();
        assert!(maybe_revert_reason(&encoded).is_none());
    }

    #[test]
    fn decode_revert_reason_with_invalid_selector() {
        // Test data that doesn't start with revert or panic selector
        let invalid_data = vec![0x12, 0x34, 0x56, 0x78]; // Invalid selector
        assert_eq!(
            maybe_revert_reason(&invalid_data),
            None,
            "Should return None for invalid selector"
        );

        // Test data too short
        let short_data = vec![0x08, 0xc3]; // Only 2 bytes
        assert_eq!(maybe_revert_reason(&short_data), None, "Should return None for data too short");

        // Test valid string but with proper selector validation
        let control_char_err = GenericContractError::Revert("\u{000f}.[l".into());
        let encoded = control_char_err.abi_encode();
        // This should now return the string since we're using Geth's validation logic
        assert_eq!(maybe_revert_reason(&encoded), Some("\u{000f}.[l".to_string()));

        // Test normal readable string
        let readable_err = GenericContractError::Revert("Normal error message".into());
        let encoded = readable_err.abi_encode();
        assert_eq!(maybe_revert_reason(&encoded), Some("Normal error message".to_string()));
    }

    #[test]
    fn decode_revert_reason_with_raw_string() {
        let non_readable_data = "\u{9a62}\u{0002}".as_bytes();
        assert_eq!(
            maybe_revert_reason(non_readable_data),
            None,
            "Should return None for raw strings"
        );
    }

    // <https://github.com/paradigmxyz/revm-inspectors/pull/353#issuecomment-36146608124>
    #[test]
    fn decode_string_revert() {
        let err = hex!(
            "0x08c379a0000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000214661696c656420746f2076657269667920424950333232207369676e617475726500000000000000000000000000000000000000000000000000000000000000"
        );
        let reason = maybe_revert_reason(&err[..]).unwrap();
        assert_eq!(reason, "Failed to verify BIP322 signature".to_string());
    }

    #[test]
    fn convert_memory_prefixes_and_zero_pads_chunks() {
        let data = (0u8..33).collect::<Vec<_>>();

        assert_eq!(
            convert_memory(&data),
            vec![
                "0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f".to_string(),
                "0x2000000000000000000000000000000000000000000000000000000000000000".to_string(),
            ]
        );
    }
}
