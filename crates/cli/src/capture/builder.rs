use super::{
    CaptureError,
    overlay::Overlay,
    parse::{parse_address, parse_b256, parse_bytes, parse_u64, parse_u256},
    rpc::{RpcAccount, RpcEndpoint},
};
use crate::capture::model::{
    AccountState, BlockHash, CapturedBlock, CapturedBlocks, CapturedCase, CapturedInput,
    CapturedVersions, Code, CodeTable, State, StorageEntry,
};
use alloy_consensus::Header;
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, U256, keccak256};
use evm2::evm::{
    BUILDER_DEPOSIT_REQUEST_ADDRESS, BUILDER_EXIT_REQUEST_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS,
    WITHDRAWAL_REQUEST_ADDRESS,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

/// The storage layout shared by the EIP-7002 and EIP-7251 request queue contracts.
const QUEUE_EXCESS_SLOT: u64 = 0;
const QUEUE_COUNT_SLOT: u64 = 1;
const QUEUE_HEAD_SLOT: u64 = 2;
const QUEUE_TAIL_SLOT: u64 = 3;
const QUEUE_OFFSET_SLOT: u64 = 4;
/// Slots per queued entry: source address, then the 48 byte pubkey and amount (EIP-7002).
const WITHDRAWAL_ENTRY_SLOTS: u64 = 3;
/// Slots per queued entry: source address, then the source and target pubkeys (EIP-7251).
const CONSOLIDATION_ENTRY_SLOTS: u64 = 4;
/// The consensus layer drops consolidation requests beyond this queue length (EIP-7251).
const MAX_QUEUE_LEN: u64 = 262_144;

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

    pub(super) async fn capture_block_hashes(
        &mut self,
        rpc: &RpcEndpoint,
        first_block: u64,
    ) -> Result<(), CaptureError> {
        let start = first_block.saturating_sub(256);
        let mut block_hashes = stream::iter(start..first_block)
            .map(|number| async move {
                let hash = rpc.block_hash(number).await?;
                Ok::<_, CaptureError>((number, hash))
            })
            .buffer_unordered(rpc.max_concurrent_requests());

        while let Some(block_hash) = block_hashes.next().await {
            let (number, hash) = block_hash?;
            self.block_hashes.insert(number, hash);
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

    /// Captures the system contracts that the blocks call after their transactions.
    ///
    /// Block execution requires their code and, for the request queues, their queue storage, but
    /// transaction traces never touch them at the state that the system calls start from. The
    /// base pre-state therefore reads them as of the block before the first captured block and
    /// replaces anything the traces recorded: a trace sees the contract after the system calls of
    /// earlier blocks in the range, which replay executes itself. The builder request contracts
    /// are captured with code only: their storage stays empty.
    pub(super) async fn capture_system_contracts(
        &mut self,
        rpc: &RpcEndpoint,
        parent_number: u64,
        spec: evm2::SpecId,
    ) -> Result<(), CaptureError> {
        for contract in system_contracts(spec) {
            let account = rpc.account(contract.address, parent_number).await?;
            let storage = match contract.queue_entry_slots {
                Some(entry_slots) => {
                    read_queue_storage(rpc, contract.address, entry_slots, parent_number).await?
                }
                None => Vec::new(),
            };
            self.capture_system_contract(contract.address, account, storage)?;
        }
        Ok(())
    }

    fn capture_system_contract(
        &mut self,
        address: Address,
        account: RpcAccount,
        storage: Vec<StorageEntry>,
    ) -> Result<(), CaptureError> {
        let code_hash = self.insert_code(account.code)?;
        let state = self.account_state(address);
        state.balance = account.balance;
        state.nonce = account.nonce;
        state.code_hash = code_hash;
        state.storage = storage;
        self.captured_balances.insert(address);
        self.captured_nonces.insert(address);
        self.captured_codes.insert(address);
        Ok(())
    }

    pub(super) fn finish(
        self,
        parent_header: Header,
        mut block_inputs: Vec<CapturedBlock>,
    ) -> CapturedCase {
        let input = if block_inputs.len() == 1 {
            CapturedInput::Block(Box::new(block_inputs.pop().expect("single block exists")))
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
            parent_header,
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
                    let traced_balance = parse_u256(balance)?;
                    if overlay.account_exists(&address) && traced_balance.is_zero() {
                        return Ok(());
                    }
                    let withdrawal_balance = overlay.withdrawal_balance(&address);
                    let balance = traced_balance.checked_sub(withdrawal_balance).ok_or(
                        CaptureError::WithdrawalBalanceUnderflow {
                            address,
                            traced_balance,
                            withdrawal_balance,
                        },
                    )?;
                    if balance.is_zero() && withdrawal_balance != U256::ZERO {
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
                    let code_hash = self.insert_code(parse_bytes(code)?)?;
                    self.account_state(address).code_hash = code_hash;
                    self.captured_codes.insert(address);
                }
            }
        }

        if block_number == 0 && tx_index == 0 {
            return Ok(());
        }
        Ok(())
    }

    fn insert_code(&mut self, bytecode: Bytes) -> Result<B256, CaptureError> {
        let code_hash = if bytecode.is_empty() { KECCAK256_EMPTY } else { keccak256(&bytecode) };
        match self.codes.entry(code_hash) {
            Entry::Vacant(entry) => {
                entry.insert(bytecode);
            }
            Entry::Occupied(entry) if entry.get() == &bytecode => {}
            Entry::Occupied(_) => return Err(CaptureError::CodeHashCollision { code_hash }),
        }
        Ok(code_hash)
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

/// A system contract that a block calls after its transactions and requires to have code.
struct SystemContract {
    address: Address,
    /// The storage slots of one entry of its request queue, if it keeps the EIP-7002 queue layout.
    queue_entry_slots: Option<u64>,
}

/// Returns the system contracts that a block of this spec calls after its transactions.
fn system_contracts(spec: evm2::SpecId) -> Vec<SystemContract> {
    let mut contracts = Vec::new();
    if spec.enables(evm2::SpecId::PRAGUE) {
        contracts.push(SystemContract {
            address: WITHDRAWAL_REQUEST_ADDRESS,
            queue_entry_slots: Some(WITHDRAWAL_ENTRY_SLOTS),
        });
        contracts.push(SystemContract {
            address: CONSOLIDATION_REQUEST_ADDRESS,
            queue_entry_slots: Some(CONSOLIDATION_ENTRY_SLOTS),
        });
    }
    if spec.enables(evm2::SpecId::AMSTERDAM) {
        contracts.push(SystemContract {
            address: BUILDER_DEPOSIT_REQUEST_ADDRESS,
            queue_entry_slots: None,
        });
        contracts.push(SystemContract {
            address: BUILDER_EXIT_REQUEST_ADDRESS,
            queue_entry_slots: None,
        });
    }
    contracts
}

/// Reads the non-zero storage that a system call of a request queue contract starts from.
async fn read_queue_storage(
    rpc: &RpcEndpoint,
    address: Address,
    entry_slots: u64,
    block_number: u64,
) -> Result<Vec<StorageEntry>, CaptureError> {
    let (head, tail) = tokio::try_join!(
        rpc.storage(address, U256::from(QUEUE_HEAD_SLOT), block_number),
        rpc.storage(address, U256::from(QUEUE_TAIL_SLOT), block_number),
    )?;
    let slots = queue_slots(address, entry_slots, head, tail)?;
    let mut entries = stream::iter(slots)
        .map(|slot| async move {
            let value = rpc.storage(address, slot, block_number).await?;
            Ok::<_, CaptureError>(StorageEntry { slot: B256::from(slot), value })
        })
        .buffer_unordered(rpc.max_concurrent_requests())
        .try_collect::<Vec<_>>()
        .await?;
    entries.retain(|entry| !entry.value.is_zero());
    entries.sort_by_key(|entry| entry.slot);
    Ok(entries)
}

/// Returns every storage slot of an EIP-7002 style request queue contract that a system call
/// can read or that a later one will: the excess, count, head and tail slots, and all slots of
/// the entries from the head up to the tail.
fn queue_slots(
    address: Address,
    entry_slots: u64,
    head: U256,
    tail: U256,
) -> Result<Vec<U256>, CaptureError> {
    let invalid = || CaptureError::InvalidQueuePointers { address, head, tail };
    let head = u64::try_from(head).map_err(|_| invalid())?;
    let tail = u64::try_from(tail).map_err(|_| invalid())?;
    if head > tail || tail - head > MAX_QUEUE_LEN {
        return Err(invalid());
    }
    let mut slots = vec![
        U256::from(QUEUE_EXCESS_SLOT),
        U256::from(QUEUE_COUNT_SLOT),
        U256::from(QUEUE_HEAD_SLOT),
        U256::from(QUEUE_TAIL_SLOT),
    ];
    for index in head..tail {
        let first = index
            .checked_mul(entry_slots)
            .and_then(|offset| offset.checked_add(QUEUE_OFFSET_SLOT))
            .ok_or_else(invalid)?;
        let end = first.checked_add(entry_slots).ok_or_else(invalid)?;
        slots.extend((first..end).map(U256::from));
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account_trace(address: Address, account: Value) -> Value {
        let mut accounts = Map::new();
        accounts.insert(address.to_string(), account);
        Value::Object(accounts)
    }

    #[test]
    fn balance_capture_subtracts_prior_withdrawals() {
        let address = Address::repeat_byte(0x11);
        let mut overlay = Overlay::default();
        overlay.apply_withdrawal(address, U256::from(7));
        let mut builder = CaptureBuilder::mainnet();
        let pre = account_trace(address, json!({ "balance": "0xa", "nonce": 1 }));

        builder.capture_base_requirements(1, 0, &pre, &overlay).unwrap();

        let account = builder.accounts.get(&address).unwrap();
        assert_eq!(account.balance, U256::from(3));
        assert_eq!(account.nonce, 1);
    }

    #[test]
    fn balance_capture_skips_pure_withdrawal_balance() {
        let address = Address::repeat_byte(0x11);
        let mut overlay = Overlay::default();
        overlay.apply_withdrawal(address, U256::from(7));
        let mut builder = CaptureBuilder::mainnet();
        let pre = account_trace(address, json!({ "balance": "0x7" }));

        builder.capture_base_requirements(1, 0, &pre, &overlay).unwrap();

        assert!(!builder.accounts.contains_key(&address));
        assert!(!builder.captured_balances.contains(&address));
    }

    #[test]
    fn system_contract_capture_replaces_what_a_trace_recorded() {
        let address = WITHDRAWAL_REQUEST_ADDRESS;
        let mut builder = CaptureBuilder::mainnet();
        let pre = account_trace(
            address,
            json!({
                "balance": "0x1",
                "nonce": 1,
                "code": "0x60",
                "storage": { B256::with_last_byte(1).to_string(): "0x5" },
            }),
        );
        builder.capture_base_requirements(1, 0, &pre, &Overlay::default()).unwrap();

        let account =
            RpcAccount { balance: U256::from(9), nonce: 2, code: Bytes::from_static(&[1]) };
        let storage = vec![StorageEntry { slot: B256::with_last_byte(2), value: U256::from(3) }];
        builder.capture_system_contract(address, account, storage).unwrap();

        let account = builder.accounts.get(&address).unwrap();
        assert_eq!(account.balance, U256::from(9));
        assert_eq!(account.nonce, 2);
        assert_eq!(builder.codes.get(&account.code_hash).unwrap().as_ref(), &[1]);
        assert_eq!(account.storage.len(), 1);
        assert_eq!(account.storage[0].slot, B256::with_last_byte(2));
    }

    #[test]
    fn system_contract_capture_reads_an_untouched_contract() {
        let address = WITHDRAWAL_REQUEST_ADDRESS;
        let mut builder = CaptureBuilder::mainnet();

        let account =
            RpcAccount { balance: U256::from(9), nonce: 1, code: Bytes::from_static(&[1]) };
        builder.capture_system_contract(address, account, Vec::new()).unwrap();

        let account = builder.accounts.get(&address).unwrap();
        assert_eq!(account.balance, U256::from(9));
        assert_eq!(account.nonce, 1);
        assert_eq!(builder.codes.get(&account.code_hash).unwrap().as_ref(), &[1]);
    }

    #[test]
    fn system_contracts_follow_the_spec() {
        assert!(system_contracts(evm2::SpecId::CANCUN).is_empty());
        assert_eq!(system_contracts(evm2::SpecId::OSAKA).len(), 2);
        assert_eq!(system_contracts(evm2::SpecId::AMSTERDAM).len(), 4);
    }

    #[test]
    fn queue_slots_cover_every_entry_between_head_and_tail() {
        let address = WITHDRAWAL_REQUEST_ADDRESS;
        let slots = |entry_slots, head, tail| {
            queue_slots(address, entry_slots, U256::from(head), U256::from(tail))
                .unwrap()
                .into_iter()
                .map(|slot| slot.to::<u64>())
                .collect::<Vec<_>>()
        };

        assert_eq!(slots(3, 0, 0), [0, 1, 2, 3]);
        // Entries 2 and 3 of three slots each start at slots 10 and 13.
        assert_eq!(slots(3, 2, 4), [0, 1, 2, 3, 10, 11, 12, 13, 14, 15]);
        // Entry 1 of four slots starts at slot 8.
        assert_eq!(slots(4, 1, 2), [0, 1, 2, 3, 8, 9, 10, 11]);
    }

    #[test]
    fn queue_slots_reject_a_queue_beyond_the_limit() {
        let err =
            queue_slots(WITHDRAWAL_REQUEST_ADDRESS, 3, U256::ZERO, U256::from(MAX_QUEUE_LEN + 1));

        assert!(matches!(err, Err(CaptureError::InvalidQueuePointers { .. })));
    }

    #[test]
    fn queue_slots_reject_a_head_past_the_tail() {
        let err = queue_slots(WITHDRAWAL_REQUEST_ADDRESS, 3, U256::from(2), U256::from(1));

        assert!(matches!(err, Err(CaptureError::InvalidQueuePointers { .. })));
    }

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
