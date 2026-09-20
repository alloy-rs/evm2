//! Tags describing fuzzer cases.

use serde::{Deserialize, Serialize};
use std::fmt;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
    #[serde(transparent)]
    pub(crate) struct FuzzFeatures: u128 {
        const BLOB = 1 << 0;
        const CALL = 1 << 1;
        const CALLCODE = 1 << 2;
        const CLZ = 1 << 3;
        const COPY = 1 << 4;
        const CREATE = 1 << 5;
        const CREATE2 = 1 << 6;
        const DELEGATECALL = 1 << 7;
        const DUP_SWAP = 1 << 8;
        const EIP7702_AUTH = 1 << 9;
        const EIP7702_AUTH_ALT_DELEGATE = 1 << 10;
        const EIP7702_AUTH_BAD_CHAIN = 1 << 11;
        const EIP7702_AUTH_BAD_NONCE = 1 << 12;
        const EIP7702_AUTH_BAD_SIGNATURE = 1 << 13;
        const EIP7702_AUTH_EMPTY = 1 << 14;
        const EIP7702_AUTH_MULTI = 1 << 15;
        const EIP7702_AUTHORITY_BAD_NONCE = 1 << 16;
        const EIP7702_AUTHORITY_DELEGATED = 1 << 17;
        const EIP7702_AUTHORITY_MISSING = 1 << 18;
        const EIP7702_AUTHORITY_REGULAR_CODE = 1 << 19;
        const EIP7702_AUTHORITY_VALID = 1 << 20;
        const ENVIRONMENT = 1 << 21;
        const EXTERNAL_ACCOUNT = 1 << 22;
        const FORK_INVALID_OPCODE = 1 << 23;
        const FORK_INVALID_TX = 1 << 24;
        const INVALID = 1 << 25;
        const JUMP = 1 << 26;
        const KECCAK256 = 1 << 27;
        const LOG = 1 << 28;
        const MEMORY = 1 << 29;
        const MODULAR_ARITHMETIC = 1 << 30;
        const PRECOMPILE_BLAKE2F = 1 << 31;
        const PRECOMPILE_BLS12_G1_ADD = 1 << 32;
        const PRECOMPILE_BLS12_G1_MSM = 1 << 33;
        const PRECOMPILE_BLS12_G2_ADD = 1 << 34;
        const PRECOMPILE_BLS12_G2_MSM = 1 << 35;
        const PRECOMPILE_BLS12_MAP_FP2_TO_G2 = 1 << 36;
        const PRECOMPILE_BLS12_MAP_FP_TO_G1 = 1 << 37;
        const PRECOMPILE_BLS12_PAIRING = 1 << 38;
        const PRECOMPILE_BN254_ADD = 1 << 39;
        const PRECOMPILE_BN254_MUL = 1 << 40;
        const PRECOMPILE_BN254_PAIRING = 1 << 41;
        const PRECOMPILE_CALL = 1 << 42;
        const PRECOMPILE_CALL_OP = 1 << 43;
        const PRECOMPILE_DIRECT_TX = 1 << 44;
        const PRECOMPILE_ECRECOVER = 1 << 45;
        const PRECOMPILE_FUTURE_ADDRESS = 1 << 46;
        const PRECOMPILE_IDENTITY = 1 << 47;
        const PRECOMPILE_INPUT_ARBITRARY = 1 << 48;
        const PRECOMPILE_INPUT_EMPTY = 1 << 49;
        const PRECOMPILE_INPUT_EXACT = 1 << 50;
        const PRECOMPILE_INPUT_LONG = 1 << 51;
        const PRECOMPILE_INPUT_SHORT = 1 << 52;
        const PRECOMPILE_KZG_POINT_EVALUATION = 1 << 53;
        const PRECOMPILE_MODEXP = 1 << 54;
        const PRECOMPILE_P256VERIFY = 1 << 55;
        const PRECOMPILE_RIPEMD160 = 1 << 56;
        const PRECOMPILE_SHA256 = 1 << 57;
        const PRECOMPILE_STATICCALL = 1 << 58;
        const PRECOMPILE_UNKNOWN = 1 << 59;
        const PUSH = 1 << 60;
        const PUSH0 = 1 << 61;
        const RELATIVE_STACK = 1 << 62;
        const RETURN = 1 << 63;
        const RETURNDATA = 1 << 64;
        const REVERT = 1 << 65;
        const SELFDESTRUCT = 1 << 66;
        const SHIFT = 1 << 67;
        const SIGNED_ARITHMETIC = 1 << 68;
        const SLOTNUM = 1 << 69;
        const STATICCALL = 1 << 70;
        const STORAGE = 1 << 71;
        const TRANSIENT_STORAGE = 1 << 72;
        const TRUNCATED_PUSH = 1 << 73;
        const TX_CREATE = 1 << 74;
        const WIDE_PUSH = 1 << 75;
    }
}

impl fmt::Display for FuzzFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        bitflags::parser::to_writer(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_bits_and_json() {
        let features = FuzzFeatures::all();
        let json = serde_json::to_string(&features).unwrap();
        assert_eq!(serde_json::from_str::<FuzzFeatures>(&json).unwrap(), features);
        assert_eq!(serde_json::to_string(&FuzzFeatures::default()).unwrap(), r#""""#);

        let features = serde_json::from_str::<FuzzFeatures>(r#""PUSH | CALL | PUSH""#).unwrap();
        assert_eq!(serde_json::to_string(&features).unwrap(), r#""CALL | PUSH""#);
    }
}
