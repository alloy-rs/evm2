//! Alloy BAL types conversions.

// Re-export Alloy BAL types.
pub use alloy_eip7928::{
    AccountChanges as AlloyAccountChanges, BalanceChange as AlloyBalanceChange,
    BlockAccessList as AlloyBal, CodeChange as AlloyCodeChange, NonceChange as AlloyNonceChange,
    StorageChange as AlloyStorageChange,
};

use super::{AccountBal, Bal};
use crate::bytecode::BytecodeDecodeError;
use alloy_primitives::map::AddressMap;

impl TryFrom<&[AlloyAccountChanges]> for Bal {
    type Error = BytecodeDecodeError;

    /// Convert borrowed EIP-7928 [`AlloyAccountChanges`] into a [`Bal`] without consuming
    /// the source.
    ///
    /// # Errors
    ///
    /// Returns [`BytecodeDecodeError`] if any account code change contains bytecode
    /// rejected by [`Bytecode::new_raw_checked`](crate::bytecode::Bytecode::new_raw_checked). This currently happens for malformed
    /// EIP-7702 bytecode, such as bytes with the EIP-7702 magic prefix but an invalid
    /// length or unsupported version.
    #[inline]
    fn try_from(alloy_bal: &[AlloyAccountChanges]) -> Result<Self, Self::Error> {
        let mut accounts =
            AddressMap::with_capacity_and_hasher(alloy_bal.len(), Default::default());
        for alloy_account in alloy_bal {
            accounts.insert(alloy_account.address, AccountBal::try_from(alloy_account)?);
        }

        Ok(Self { accounts })
    }
}

impl TryFrom<AlloyBal> for Bal {
    type Error = BytecodeDecodeError;

    /// Convert an EIP-7928 [`AlloyBal`] into a [`Bal`].
    ///
    /// # Errors
    ///
    /// Returns [`BytecodeDecodeError`] if any account code change contains bytecode
    /// rejected by [`Bytecode::new_raw_checked`](crate::bytecode::Bytecode::new_raw_checked). This currently happens for malformed
    /// EIP-7702 bytecode, such as bytes with the EIP-7702 magic prefix but an invalid
    /// length or unsupported version.
    #[inline]
    fn try_from(alloy_bal: AlloyBal) -> Result<Self, Self::Error> {
        let mut accounts =
            AddressMap::with_capacity_and_hasher(alloy_bal.len(), Default::default());
        for alloy_account in alloy_bal {
            let address = alloy_account.address;
            accounts.insert(address, AccountBal::try_from(alloy_account)?);
        }

        Ok(Self { accounts })
    }
}
