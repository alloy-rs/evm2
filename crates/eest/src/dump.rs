//! Post-execution state dump.
//!
//! Renders every live account in an [`InMemoryDB`] — balance, nonce, code, and
//! non-zero storage — as an aligned, human-readable text block, ordered by
//! address (and by slot within each account) for stable, diffable output.

use alloy_primitives::{B256, U256};
use evm2::evm::InMemoryDB;
use std::{collections::BTreeMap, io::Write};

/// Writes a pretty state dump of `db` to `out`, headed by `state_root`.
pub(crate) fn dump_state<W: Write>(out: &mut W, db: &InMemoryDB, state_root: B256) {
    let accounts: BTreeMap<_, _> = db
        .cache
        .accounts
        .iter()
        .filter_map(|(address, info)| info.as_ref().map(|info| (*address, info)))
        .collect();

    let _ = writeln!(out, "state root {state_root} ({})", plural(accounts.len(), "account"));
    for (address, info) in accounts {
        let _ = writeln!(out, "account {address}");
        let _ = writeln!(out, "  balance {}", info.balance);
        let _ = writeln!(out, "  nonce   {}", info.nonce);

        let code_len = info
            .code
            .as_ref()
            .or_else(|| db.cache.contracts.get(&info.code_hash))
            .map(|code| code.original_byte_slice().len())
            .unwrap_or_default();
        if code_len != 0 {
            let _ =
                writeln!(out, "  code    {}, hash {}", plural(code_len, "byte"), info.code_hash);
        }

        let slots: BTreeMap<U256, U256> = db
            .cache
            .storage
            .get(&address)
            .into_iter()
            .flat_map(|storage| storage.slots.iter())
            .filter(|(_, value)| !value.is_zero())
            .map(|(&key, &value)| (key, value))
            .collect();
        if !slots.is_empty() {
            let _ = writeln!(out, "  storage ({})", plural(slots.len(), "slot"));
            for (key, value) in slots {
                let _ = writeln!(out, "    {key:#x} = {value:#x}");
            }
        }
    }
}

/// Formats `count` with `noun`, appending `s` unless the count is exactly one.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 { format!("1 {noun}") } else { format!("{count} {noun}s") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{insert_account_with_storage, parse_bytecode};
    use alloy_primitives::{Address, Bytes};
    use evm2::evm::{AccountInfo, InMemoryDB};

    #[test]
    fn dumps_accounts_and_non_zero_storage() {
        let mut db = InMemoryDB::default();
        let address = Address::from([0x11; 20]);
        let mut info = AccountInfo::default().with_code(parse_bytecode(Bytes::new()));
        info.balance = U256::from(1_000u64);
        info.nonce = 7;
        insert_account_with_storage(
            &mut db,
            address,
            info,
            [(U256::from(1u64), U256::from(42u64)), (U256::from(2u64), U256::ZERO)],
        );

        let mut buf = Vec::new();
        dump_state(&mut buf, &db, B256::ZERO);
        let text = String::from_utf8(buf).unwrap();

        assert!(text.contains(&format!("account {address}")));
        assert!(text.contains("balance 1000"));
        assert!(text.contains("nonce   7"));
        assert!(text.contains("storage (1 slot)"));
        assert!(text.contains("0x1 = 0x2a"));
        // The zero-valued slot is pruned, and an empty-code account has no code line.
        assert!(!text.contains("0x2 ="));
        assert!(!text.contains("code"));
    }
}
