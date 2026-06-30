use super::{Bytecode, BytecodeKind};
use alloy_primitives::{Address, Bytes};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum BytecodeSerde {
    LegacyAnalyzed { bytecode: Bytes, original_len: usize },
    Eip7702 { delegated_address: Address },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BytecodeDeserialize {
    New(Bytes),
    Old(BytecodeSerde),
}

impl Serialize for Bytecode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let repr = match self.kind() {
            BytecodeKind::Legacy => BytecodeSerde::LegacyAnalyzed {
                bytecode: self.bytes().clone(),
                original_len: self.len(),
            },
            BytecodeKind::Eip7702 => BytecodeSerde::Eip7702 {
                delegated_address: self.eip7702_address().expect("EIP-7702 bytecode has address"),
            },
        };
        repr.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Bytecode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BytecodeDeserialize::deserialize(deserializer)? {
            BytecodeDeserialize::New(bytes) => {
                Self::new_raw_checked(bytes).map_err(serde::de::Error::custom)
            }
            BytecodeDeserialize::Old(bytecode_serde) => match bytecode_serde {
                BytecodeSerde::LegacyAnalyzed { bytecode, original_len } => {
                    if original_len > bytecode.len() {
                        return Err(serde::de::Error::custom(
                            "original_len is greater than bytecode length",
                        ));
                    }
                    Ok(Self::new_legacy(bytecode.slice(..original_len)))
                }
                BytecodeSerde::Eip7702 { delegated_address } => {
                    Ok(Self::new_eip7702(delegated_address))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{EIP7702_BYTECODE_LEN, EIP7702_MAGIC_BYTES, EIP7702_VERSION};

    #[test]
    fn serde_roundtrip_preserves_legacy_eip7702_prefix() {
        let mut raw = [0u8; EIP7702_BYTECODE_LEN];
        raw[..2].copy_from_slice(EIP7702_MAGIC_BYTES);
        raw[2] = EIP7702_VERSION;
        raw[3..].copy_from_slice(Address::with_last_byte(0x7a).as_slice());

        let bytecode = Bytecode::new_legacy(Bytes::copy_from_slice(&raw));
        assert_eq!(bytecode.kind(), BytecodeKind::Legacy);

        let json = serde_json::to_string(&bytecode).unwrap();
        let deserialized: Bytecode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.kind(), BytecodeKind::Legacy);
        assert_eq!(deserialized.original_byte_slice(), raw);
    }

    #[test]
    fn serde_roundtrip_preserves_eip7702_kind() {
        let delegated_address = Address::with_last_byte(0x42);
        let bytecode = Bytecode::new_eip7702(delegated_address);

        let json = serde_json::to_string(&bytecode).unwrap();
        let deserialized: Bytecode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.kind(), BytecodeKind::Eip7702);
        assert_eq!(deserialized.eip7702_address(), Some(delegated_address));
    }
}
