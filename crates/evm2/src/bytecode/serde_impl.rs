use super::Bytecode;
use alloy_primitives::{Address, Bytes};
use serde::{Deserialize, Serialize};

// TODO: eventually remove `BytecodeSerdeOld`.

#[derive(Deserialize)]
#[serde(untagged)]
enum BytecodeSerde {
    New(Bytes),
    Old(BytecodeSerdeOld),
}

#[derive(Deserialize)]
enum BytecodeSerdeOld {
    LegacyAnalyzed {
        bytecode: Bytes,
        original_len: usize,
        #[allow(dead_code)]
        jump_table: serde::de::IgnoredAny,
    },
    Eip7702 {
        delegated_address: Address,
    },
}

impl Serialize for Bytecode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.original_bytes().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Bytecode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BytecodeSerde::deserialize(deserializer)? {
            BytecodeSerde::New(bytes) => {
                Self::new_raw_checked(bytes).map_err(serde::de::Error::custom)
            }
            BytecodeSerde::Old(bytecode_serde_old) => match bytecode_serde_old {
                BytecodeSerdeOld::LegacyAnalyzed { bytecode, original_len, jump_table: _ } => {
                    if original_len > bytecode.len() {
                        return Err(serde::de::Error::custom(
                            "original_len is greater than bytecode length",
                        ));
                    }
                    Ok(Self::new_legacy(bytecode.slice(..original_len)))
                }
                BytecodeSerdeOld::Eip7702 { delegated_address } => {
                    Ok(Self::new_eip7702(delegated_address))
                }
            },
        }
    }
}
