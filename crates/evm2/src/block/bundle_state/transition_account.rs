use super::{
    AccountInfoRevert, AccountRevert, AccountStatus, BundleAccount, RevertToSlot,
    StorageWithOriginalValues,
};
use crate::{bytecode::Bytecode, evm::AccountInfo};
use alloy_primitives::{B256, U256};

/// Account Created when EVM state is merged to cache state.
/// And it is sent to Block state.
///
/// It is used when block state gets merged to bundle state to
/// create needed Reverts.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TransitionAccount<S = StorageWithOriginalValues> {
    /// Account information, if account exists.
    pub info: Option<AccountInfo>,
    /// Current account status.
    pub status: AccountStatus,
    /// Previous account info is needed for account that got initially loaded.
    /// Initially loaded account are not present inside bundle and are needed
    /// to generate Reverts.
    pub previous_info: Option<AccountInfo>,
    /// Mostly needed when previous status Loaded/LoadedEmpty.
    pub previous_status: AccountStatus,
    /// Storage contains both old and new account
    pub storage: S,
    /// If there is transition that clears the storage we should mark it here and
    /// delete all storages in BundleState. This flag is needed if we have transition
    /// between Destroyed states from DestroyedChanged-> DestroyedAgain-> DestroyedChanged
    /// in the end transition that we would have would be `DestroyedChanged->DestroyedChanged`
    /// and with only that info we couldn't decide what to do.
    pub storage_was_destroyed: bool,
}

impl TransitionAccount {
    /// Create new LoadedEmpty account.
    pub fn new_empty_eip161(storage: StorageWithOriginalValues) -> Self {
        Self {
            info: Some(AccountInfo::default()),
            status: AccountStatus::InMemoryChange,
            previous_info: None,
            previous_status: AccountStatus::LoadedNotExisting,
            storage,
            storage_was_destroyed: false,
        }
    }

    /// Return new contract bytecode if it is changed or newly created.
    pub fn has_new_contract(&self) -> Option<(B256, &Bytecode)> {
        let present_new_codehash = self.info.as_ref().map(|info| &info.code_hash);
        let previous_codehash = self.previous_info.as_ref().map(|info| &info.code_hash);
        if present_new_codehash != previous_codehash {
            return self
                .info
                .as_ref()
                .and_then(|info| info.code.as_ref().map(|c| (info.code_hash, c)));
        }
        None
    }

    /// Return the balance of account before transition.
    pub fn previous_balance(&self) -> U256 {
        self.previous_info.as_ref().map(|info| info.balance).unwrap_or_default()
    }

    /// Return the balance of account after transition.
    pub fn current_balance(&self) -> U256 {
        self.info.as_ref().map(|info| info.balance).unwrap_or_default()
    }

    /// Consume Self and create account revert from it.
    pub fn create_revert(self) -> Option<AccountRevert> {
        let mut previous_account = self.original_bundle_account();
        previous_account.update_and_create_revert(self)
    }

    /// Present bundle account
    pub fn present_bundle_account(&self) -> BundleAccount {
        BundleAccount {
            info: self.info.clone(),
            original_info: self.previous_info.clone(),
            storage: self.storage.clone(),
            status: self.status,
        }
    }

    /// Returns the present bundle account and the revert of an account the bundle does not hold
    /// yet, as [`Self::present_bundle_account`] and [`Self::create_revert`] would, without copying the
    /// storage of a changed account.
    pub fn into_present_and_revert(self) -> (BundleAccount, Option<AccountRevert>) {
        let in_memory = match (self.status, self.previous_status) {
            (
                AccountStatus::Changed,
                AccountStatus::Changed | AccountStatus::Loaded | AccountStatus::LoadedEmptyEIP161,
            ) => false,
            (
                AccountStatus::InMemoryChange,
                AccountStatus::Loaded
                | AccountStatus::InMemoryChange
                | AccountStatus::LoadedEmptyEIP161
                | AccountStatus::LoadedNotExisting,
            ) => true,
            _ => return (self.present_bundle_account(), self.create_revert()),
        };
        let account = if in_memory && self.previous_status == AccountStatus::LoadedNotExisting {
            AccountInfoRevert::DeleteIt
        } else if self.previous_info != self.info {
            AccountInfoRevert::RevertTo(self.previous_info.clone().unwrap_or_default())
        } else {
            AccountInfoRevert::DoNothing
        };
        let storage = self
            .storage
            .iter()
            .filter(|(_, slot)| slot.is_changed())
            .map(|(key, slot)| (*key, RevertToSlot::Some(slot.original_value())))
            .collect();
        let revert = AccountRevert {
            account,
            storage,
            previous_status: self.previous_status,
            wipe_storage: false,
        };
        let present = BundleAccount {
            info: self.info,
            original_info: self.previous_info,
            storage: self.storage,
            status: self.status,
        };
        (present, Some(revert))
    }

    /// Original bundle account
    fn original_bundle_account(&self) -> BundleAccount {
        BundleAccount {
            info: self.previous_info.clone(),
            original_info: self.previous_info.clone(),
            storage: StorageWithOriginalValues::default(),
            status: self.previous_status,
        }
    }
}

impl<S> TransitionAccount<S> {
    /// Map the storage of the transition account.
    pub fn map_storage<F, N>(self, f: F) -> TransitionAccount<N>
    where
        F: FnOnce(S) -> N,
    {
        let Self { info, status, previous_info, previous_status, storage, storage_was_destroyed } =
            self;
        TransitionAccount {
            info,
            status,
            previous_info,
            previous_status,
            storage: f(storage),
            storage_was_destroyed,
        }
    }
}
