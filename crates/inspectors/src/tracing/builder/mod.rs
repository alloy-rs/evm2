//! Builder types for building traces

use alloc::vec::Vec;
use alloy_primitives::{Address, U256, map::U256Map};
use evm2::evm::{AccountInfo, DbResult, DynDatabase, StateChanges, Tracked};

/// A per-account state change combining the account-info delta with its storage-slot deltas.
///
/// evm2's [`StateChanges`] tracks account-info and storage changes in separate maps and omits
/// accounts whose info did not change (for example a contract that only wrote storage). This view
/// re-bundles them per address — resolving the unchanged account info from the database for
/// storage-only accounts — so the geth/parity builders can iterate a single per-account change,
/// mirroring the bundled change type they were written against.
pub(crate) struct AccountChange<'a> {
    /// Account info after the transaction, or `None` if the account was deleted.
    pub(crate) current: Option<AccountInfo>,
    /// Storage slot changes for the account, if any.
    storage: Option<&'a U256Map<Tracked<U256>>>,
    created: bool,
    selfdestructed: bool,
}

impl<'a> AccountChange<'a> {
    /// Whether the account was created during the transaction.
    pub(crate) const fn is_created(&self) -> bool {
        self.created
    }

    /// Whether the account was deleted (self-destructed) during the transaction.
    pub(crate) const fn is_selfdestructed(&self) -> bool {
        self.selfdestructed
    }

    /// Iterates the account's changed storage slots.
    pub(crate) fn storage(&self) -> impl Iterator<Item = (&U256, &Tracked<U256>)> {
        self.storage.into_iter().flat_map(|slots| slots.iter())
    }
}

/// Bundles [`StateChanges`] into per-account [`AccountChange`]s over the union of accounts that had
/// an info change and accounts that only had storage changes.
pub(crate) fn account_changes<'a>(
    state: &'a StateChanges,
    db: &mut dyn DynDatabase,
) -> DbResult<Vec<(Address, AccountChange<'a>)>> {
    let mut changes = Vec::with_capacity(state.accounts.len());
    for (&address, account) in &state.accounts {
        changes.push((
            address,
            AccountChange {
                current: account.current.clone(),
                storage: state.storage.get(&address).map(|set| &set.slots),
                created: state.created.contains(&address),
                selfdestructed: account.original.is_some() && account.current.is_none(),
            },
        ));
    }
    for (&address, set) in &state.storage {
        if state.accounts.contains_key(&address) {
            continue;
        }
        // Storage-only account: its info is unchanged, so resolve it from the database.
        let current = db.get_account(&address)?;
        changes.push((
            address,
            AccountChange {
                current,
                storage: Some(&set.slots),
                created: false,
                selfdestructed: false,
            },
        ));
    }
    Ok(changes)
}

/// Geth style trace builders for `debug_` namespace
pub mod geth;

/// Parity style trace builders for `trace_` namespace
pub mod parity;

/// Walker types used for traversing various callgraphs
mod walker;
