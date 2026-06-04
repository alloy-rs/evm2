use super::{
    CaptureError, block,
    overlay::Overlay,
    parse::{parse_address, parse_b256, parse_bytes, parse_u64, parse_u256},
    rpc::{RpcEndpoint, hex_quantity},
};
use crate::capture::model::{
    AccountState, BlockHash, CapturedBlock, CapturedBlocks, CapturedCase, CapturedInput,
    CapturedVersions, Code, CodeTable, State, StorageEntry,
};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, U256, keccak256};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

#[derive(Default)]
pub(super) struct CaptureBuilder {
    versions: Vec<crate::capture::model::CapturedVersion>,
    version_indexes: BTreeMap<u32, u32>,
    codes: BTreeMap<B256, Bytes>,
    accounts: BTreeMap<Address, AccountState>,
    block_hashes: BTreeMap<u64, B256>,
    captured_balances: BTreeSet<Address>,
    captured_nonces: BTreeSet<Address>,
    captured_codes: BTreeSet<Address>,
    absent_accounts: BTreeSet<Address>,
    zero_storage_slots: BTreeSet<(Address, B256)>,
    chain_id: u64,
}

impl CaptureBuilder {
    pub(super) fn mainnet() -> Self {
        Self { chain_id: 1, ..Self::default() }
    }

    pub(super) const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub(super) fn capture_block_hashes(
        &mut self,
        rpc: &RpcEndpoint,
        first_block: u64,
    ) -> Result<(), CaptureError> {
        let start = first_block.saturating_sub(256);
        for number in start..first_block {
            let raw_block = rpc.raw_block(&hex_quantity(number))?;
            let block = block::decode_consensus_block(&raw_block)?;
            self.block_hashes.insert(block.header.number, block.header.hash_slow());
        }
        Ok(())
    }

    pub(super) fn version_index(&mut self, spec: evm2::SpecId) -> Result<u32, CaptureError> {
        let spec_id = u32::from(spec);
        if let Some(index) = self.version_indexes.get(&spec_id) {
            return Ok(*index);
        }

        let index = u32::try_from(self.versions.len())
            .map_err(|_| CaptureError::TooManyCapturedVersions)?;
        self.versions.push(crate::capture::model::captured_version(spec));
        self.version_indexes.insert(spec_id, index);
        Ok(index)
    }

    /// Decides which touched accounts and storage slots belong in the capture base pre-state.
    ///
    /// If a value is already supplied by the in-range overlay, do not add it to the capture
    /// pre-state. If it is not supplied by earlier blocks or transactions in the range, record it
    /// as required base state.
    ///
    /// For example, if block `N` creates contract `C` and block `N + 1` reads `C`, then `C` must
    /// not be stored in the base pre-state. Replay creates it before the later read. This function
    /// enforces that by checking the overlay before recording balances, nonces, code, nonzero
    /// storage slots, and explicit absent or zero probes.
    pub(super) fn capture_base_requirements(
        &mut self,
        block_number: u64,
        tx_index: usize,
        pre: &Value,
        overlay: &Overlay,
    ) -> Result<(), CaptureError> {
        let accounts = pre
            .as_object()
            .ok_or(CaptureError::InvalidTraceResult("prestate trace result was not an object"))?;

        for (address, account) in accounts {
            let address = parse_address(address)?;
            let account = account.as_object().ok_or(CaptureError::InvalidTraceResult(
                "prestate account entry was not an object",
            ))?;
            if is_absent_account_probe(account)? {
                if !overlay.account_exists(&address) {
                    self.absent_accounts.insert(address);
                    if let Some(storage) = account.get("storage").and_then(Value::as_object) {
                        for slot in storage.keys() {
                            self.zero_storage_slots.insert((address, parse_b256(slot)?));
                        }
                    }
                }
                continue;
            }
            if self.absent_accounts.contains(&address) && !overlay.account_exists(&address) {
                continue;
            }

            self.capture_account_field(
                block_number,
                tx_index,
                address,
                account,
                overlay,
                AccountField::Balance,
            )?;
            self.capture_account_field(
                block_number,
                tx_index,
                address,
                account,
                overlay,
                AccountField::Nonce,
            )?;
            self.capture_account_field(
                block_number,
                tx_index,
                address,
                account,
                overlay,
                AccountField::Code,
            )?;
            self.capture_storage(address, account, overlay)?;
        }

        Ok(())
    }

    pub(super) fn finish(self, mut block_inputs: Vec<CapturedBlock>) -> CapturedCase {
        let input = if block_inputs.len() == 1 {
            CapturedInput::Block(block_inputs.pop().expect("single block exists"))
        } else {
            CapturedInput::Blocks(CapturedBlocks { blocks: block_inputs })
        };

        CapturedCase {
            versions: CapturedVersions { versions: self.versions },
            code_table: CodeTable {
                codes: self
                    .codes
                    .into_iter()
                    .map(|(code_hash, bytecode)| Code { code_hash, bytecode })
                    .collect(),
            },
            pre_state: State {
                accounts: self.accounts.into_values().collect(),
                block_hashes: self
                    .block_hashes
                    .into_iter()
                    .map(|(number, hash)| BlockHash { number, hash })
                    .collect(),
            },
            post_state: None,
            input,
        }
    }

    fn capture_account_field(
        &mut self,
        block_number: u64,
        tx_index: usize,
        address: Address,
        account: &Map<String, Value>,
        overlay: &Overlay,
        field: AccountField,
    ) -> Result<(), CaptureError> {
        if overlay.account_field(&address, field) {
            return Ok(());
        }

        match field {
            AccountField::Balance => {
                if self.captured_balances.contains(&address) {
                    return Ok(());
                }
                if let Some(balance) = account.get("balance") {
                    let balance = parse_u256(balance)?;
                    if overlay.account_exists(&address) && balance.is_zero() {
                        return Ok(());
                    }
                    let state = self.account_state(address);
                    state.balance = balance;
                    self.captured_balances.insert(address);
                }
            }
            AccountField::Nonce => {
                if self.captured_nonces.contains(&address) {
                    return Ok(());
                }
                if let Some(nonce) = account.get("nonce") {
                    let state = self.account_state(address);
                    state.nonce = parse_u64(nonce)?;
                    self.captured_nonces.insert(address);
                }
            }
            AccountField::Code => {
                if self.captured_codes.contains(&address) {
                    return Ok(());
                }
                if let Some(code) = account.get("code") {
                    let bytecode = parse_bytes(code)?;
                    let code_hash =
                        if bytecode.is_empty() { KECCAK256_EMPTY } else { keccak256(&bytecode) };
                    match self.codes.entry(code_hash) {
                        Entry::Vacant(entry) => {
                            entry.insert(bytecode);
                        }
                        Entry::Occupied(entry) if entry.get() == &bytecode => {}
                        Entry::Occupied(_) => {
                            return Err(CaptureError::CodeHashCollision { code_hash });
                        }
                    }
                    let state = self.account_state(address);
                    state.code_hash = code_hash;
                    self.captured_codes.insert(address);
                }
            }
        }

        if block_number == 0 && tx_index == 0 {
            return Ok(());
        }
        Ok(())
    }

    fn account_state(&mut self, address: Address) -> &mut AccountState {
        self.accounts.entry(address).or_insert_with(|| AccountState {
            address,
            balance: U256::ZERO,
            nonce: 0,
            code_hash: KECCAK256_EMPTY,
            storage: Vec::new(),
        })
    }

    fn capture_storage(
        &mut self,
        address: Address,
        account: &Map<String, Value>,
        overlay: &Overlay,
    ) -> Result<(), CaptureError> {
        let Some(storage) = account.get("storage").and_then(Value::as_object) else {
            return Ok(());
        };
        for (slot, value) in storage {
            let slot = parse_b256(slot)?;
            if overlay.storage_slot(&address, &slot) {
                continue;
            }
            if self.zero_storage_slots.contains(&(address, slot)) {
                continue;
            }
            let value = parse_u256(value)?;
            if value.is_zero() {
                self.zero_storage_slots.insert((address, slot));
                continue;
            }
            let state = self.account_state(address);
            if state.storage.iter().any(|entry| entry.slot == slot) {
                continue;
            }
            state.storage.push(StorageEntry { slot, value });
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) enum AccountField {
    Balance,
    Nonce,
    Code,
}

fn is_absent_account_probe(account: &Map<String, Value>) -> Result<bool, CaptureError> {
    // Treat a zero-only observation as evidence that the account or slot was absent, not as an
    // empty account that must be materialized in the capture pre-state. For example:
    //
    // {
    //     "balance": "0x0",
    //     "storage": {
    //         "0x...": "0x0"
    //     }
    // }
    //
    // Any field outside balance/storage, or any nonzero balance/storage value, means the entry is
    // carrying real account data and must be handled by the normal capture path. In particular,
    // nonce/code fields are account metadata even when they are zero or empty; they are not
    // absence probes.
    if account.keys().any(|key| key != "balance" && key != "storage") {
        return Ok(false);
    }
    if let Some(balance) = account.get("balance")
        && !parse_u256(balance)?.is_zero()
    {
        return Ok(false);
    }
    if let Some(storage) = account.get("storage").and_then(Value::as_object) {
        for value in storage.values() {
            if !parse_u256(value)?.is_zero() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_account_probe_accepts_zero_balance() {
        let account = json!({ "balance": "0x0" });

        assert!(is_absent_account_probe(account.as_object().unwrap()).unwrap());
    }

    #[test]
    fn absent_account_probe_accepts_zero_storage() {
        let account = json!({
            "balance": "0x0",
            "storage": {
                "0x01": "0x0"
            }
        });

        assert!(is_absent_account_probe(account.as_object().unwrap()).unwrap());
    }

    #[test]
    fn absent_account_probe_rejects_nonce_even_when_zero() {
        let account = json!({ "nonce": 0 });

        assert!(!is_absent_account_probe(account.as_object().unwrap()).unwrap());
    }

    #[test]
    fn absent_account_probe_rejects_code_even_when_empty() {
        let account = json!({ "code": "0x" });

        assert!(!is_absent_account_probe(account.as_object().unwrap()).unwrap());
    }

    #[test]
    fn absent_account_probe_rejects_nonzero_balance() {
        let account = json!({ "balance": "0x1" });

        assert!(!is_absent_account_probe(account.as_object().unwrap()).unwrap());
    }

    #[test]
    fn absent_account_probe_rejects_nonzero_storage() {
        let account = json!({
            "storage": {
                "0x01": "0x1"
            }
        });

        assert!(!is_absent_account_probe(account.as_object().unwrap()).unwrap());
    }
}
