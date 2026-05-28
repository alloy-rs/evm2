use crate::{program::Program, rng::Gen};
use alloy_consensus::{
    TxEip1559, TxEip2930, TxEip4844, TxEip7702, TxLegacy,
    transaction::{Recovered, TxEip4844Variant},
};
use alloy_eips::{
    eip2930::{AccessList, AccessListItem},
    eip7702::{Authorization, SignedAuthorization},
};
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256};
use evm2::{SpecId, env::BlockEnv, ethereum::RecoveredTxEnvelope, interpreter::op};
use revm::{
    context::{BlockEnv as RevmBlockEnv, TxEnv as RevmTxEnv},
    primitives::TxKind as RevmTxKind,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, str::FromStr};

pub(crate) const CALLER: Address = Address::new([0x10; 20]);
pub(crate) const TARGET: Address = Address::new([0x20; 20]);
pub(crate) const BENEFICIARY: Address = Address::new([0x30; 20]);
const CALLER_BALANCE: U256 = U256::from_limbs([0, 0, 1, 0]);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EvmCase {
    #[serde(with = "spec_serde")]
    pub(crate) spec: SpecId,
    pub(crate) block: CaseBlock,
    pub(crate) tx: CaseTx,
    #[serde(default)]
    pub(crate) extra_txs: Vec<CaseTx>,
    pub(crate) accounts: Vec<CaseAccount>,
}

impl EvmCase {
    pub(crate) fn txs(&self) -> impl Iterator<Item = &CaseTx> {
        core::iter::once(&self.tx).chain(self.extra_txs.iter())
    }

    pub(crate) fn generate(rng: &mut Gen) -> Self {
        let spec = match rng.range(11) {
            0 => SpecId::FRONTIER,
            1 => SpecId::HOMESTEAD,
            2 => SpecId::TANGERINE,
            3 => SpecId::SPURIOUS_DRAGON,
            4 => SpecId::BYZANTIUM,
            5 => SpecId::ISTANBUL,
            6 => SpecId::BERLIN,
            7 => SpecId::LONDON,
            8 => SpecId::SHANGHAI,
            9 => SpecId::CANCUN,
            _ => SpecId::PRAGUE,
        };
        let block = CaseBlock::generate(rng, spec);
        let mut extra_accounts = Vec::new();
        for i in 0..rng.range_inclusive(0, 4) {
            let mut callee_storage = BTreeMap::new();
            if rng.one_in(2) {
                callee_storage.insert(rng.biased_word(), rng.biased_word());
            }
            extra_accounts.push(CaseAccount {
                address: Address::with_last_byte(0x40 + i as u8),
                balance: rng.small_word(10_000),
                nonce: rng.range_inclusive(0, 3) as u64,
                code: tiny_callee_code(rng, spec),
                storage: callee_storage,
            });
        }
        let mut address_pool =
            vec![CALLER, TARGET, BENEFICIARY, Address::ZERO, Address::new([0xff; 20])];
        for i in 1..=10 {
            address_pool.push(Address::with_last_byte(i));
        }
        for account in &extra_accounts {
            address_pool.push(account.address);
        }
        let mut call_pool = Vec::new();
        call_pool.push(CALLER);
        for i in 1..=4 {
            call_pool.push(Address::with_last_byte(i));
        }
        for account in &extra_accounts {
            call_pool.push(account.address);
        }
        for i in 0..3 {
            let address = Address::with_last_byte(0x80 + i);
            address_pool.push(address);
            call_pool.push(address);
        }
        let program = Program::generate(rng, spec, &address_pool, &call_pool).into_bytecode();
        let mut storage = BTreeMap::new();
        for _ in 0..rng.range_inclusive(0, 4) {
            storage.insert(rng.biased_word(), rng.biased_word());
        }
        let mut accounts = vec![
            CaseAccount {
                address: CALLER,
                balance: CALLER_BALANCE,
                nonce: 0,
                code: Bytes::new(),
                storage: BTreeMap::new(),
            },
            CaseAccount {
                address: TARGET,
                balance: U256::from(1_000_000),
                nonce: 1,
                code: program,
                storage,
            },
        ];
        accounts.extend(extra_accounts);
        let input_len = rng.range_inclusive(0, 64);
        let tx = CaseTx::generate(rng, spec, &accounts, input_len, 0);
        if tx.kind == TxKindCase::Eip7702 {
            add_eip7702_authority(&mut accounts);
        }
        let mut extra_txs = Vec::new();
        for nonce in 1..=rng.range_inclusive(0, 3) as u64 {
            let input_len = rng.range_inclusive(0, 64);
            extra_txs.push(CaseTx::generate(rng, spec, &accounts, input_len, nonce));
            if extra_txs.last().is_some_and(|tx| tx.kind == TxKindCase::Eip7702) {
                add_eip7702_authority(&mut accounts);
            }
        }
        Self { spec, block, tx, extra_txs, accounts }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaseBlock {
    pub(crate) number: U256,
    pub(crate) timestamp: U256,
    pub(crate) gas_limit: u64,
    pub(crate) basefee: u64,
}

fn tiny_callee_code(rng: &mut Gen, spec: SpecId) -> Bytes {
    match rng.range(7) {
        0 => [op::STOP].into_iter().collect::<Vec<_>>().into(),
        1 => [op::PUSH1, 0, op::PUSH1, 0, op::RETURN].into_iter().collect::<Vec<_>>().into(),
        2 if spec.enables(SpecId::BYZANTIUM) => {
            [op::PUSH1, 0, op::PUSH1, 0, op::REVERT].into_iter().collect::<Vec<_>>().into()
        }
        3 => [op::PUSH1, 1, op::PUSH1, 0, op::SSTORE, op::STOP]
            .into_iter()
            .collect::<Vec<_>>()
            .into(),
        4 => returning_callee_code(op::RETURN),
        5 if spec.enables(SpecId::BYZANTIUM) => returning_callee_code(op::REVERT),
        _ => {
            let len = rng.range_inclusive(1, 32);
            rng.bytes(len).into()
        }
    }
}

fn returning_callee_code(stop: u8) -> Bytes {
    let mut code = Vec::new();
    code.push(op::PUSH32);
    code.extend([0xab; 32]);
    code.extend([op::PUSH1, 0, op::MSTORE, op::PUSH1, 32, op::PUSH1, 0, stop]);
    code.into()
}

impl CaseBlock {
    fn generate(rng: &mut Gen, _spec: SpecId) -> Self {
        Self {
            number: rng.small_word(1_000_000),
            timestamp: rng.small_word(2_000_000_000),
            gas_limit: 30_000_000,
            basefee: 0,
        }
    }

    pub(crate) fn evm2(&self) -> BlockEnv {
        BlockEnv {
            number: self.number,
            beneficiary: BENEFICIARY,
            timestamp: self.timestamp,
            gas_limit: U256::from(self.gas_limit),
            basefee: U256::from(self.basefee),
            ..BlockEnv::default()
        }
    }

    pub(crate) fn revm(&self) -> RevmBlockEnv {
        RevmBlockEnv {
            number: self.number,
            beneficiary: BENEFICIARY,
            timestamp: self.timestamp,
            gas_limit: self.gas_limit,
            basefee: self.basefee,
            ..RevmBlockEnv::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaseTx {
    #[serde(default)]
    pub(crate) kind: TxKindCase,
    pub(crate) caller: Address,
    pub(crate) target: Address,
    pub(crate) gas_limit: u64,
    pub(crate) gas_price: u128,
    pub(crate) value: U256,
    pub(crate) input: Bytes,
    pub(crate) nonce: u64,
    #[serde(default)]
    pub(crate) access_list: AccessList,
    #[serde(default)]
    pub(crate) blob_hashes: Vec<B256>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum TxKindCase {
    #[default]
    Legacy,
    Eip2930,
    Eip1559,
    Eip4844,
    Eip7702,
}

impl TxKindCase {
    fn generate(rng: &mut Gen, spec: SpecId) -> Self {
        if rng.one_in(20)
            && let Some(kind) = Self::generate_fork_invalid(rng, spec)
        {
            return kind;
        }
        match rng.range(5) {
            0 if spec.enables(SpecId::PRAGUE) => Self::Eip7702,
            1 if spec.enables(SpecId::CANCUN) => Self::Eip4844,
            2 if spec.enables(SpecId::LONDON) => Self::Eip1559,
            3 if spec.enables(SpecId::BERLIN) => Self::Eip2930,
            _ => Self::Legacy,
        }
    }

    fn generate_fork_invalid(rng: &mut Gen, spec: SpecId) -> Option<Self> {
        let mut invalid = Vec::new();
        if !spec.enables(SpecId::BERLIN) {
            invalid.push(Self::Eip2930);
        }
        if !spec.enables(SpecId::LONDON) {
            invalid.push(Self::Eip1559);
        }
        if !spec.enables(SpecId::CANCUN) {
            invalid.push(Self::Eip4844);
        }
        if !spec.enables(SpecId::PRAGUE) {
            invalid.push(Self::Eip7702);
        }
        (!invalid.is_empty()).then(|| rng.pick(&invalid))
    }
}

fn generate_access_list(rng: &mut Gen, accounts: &[CaseAccount]) -> AccessList {
    let mut items = Vec::new();
    for _ in 0..rng.range_inclusive(0, 3) {
        let account = &accounts[rng.range(accounts.len())];
        let mut storage_keys = Vec::new();
        for key in account.storage.keys().take(rng.range_inclusive(0, 3)) {
            storage_keys.push(B256::from(key.to_be_bytes::<32>()));
        }
        if storage_keys.is_empty() && rng.one_in(2) {
            storage_keys.push(B256::from(rng.biased_word().to_be_bytes::<32>()));
        }
        items.push(AccessListItem { address: account.address, storage_keys });
    }
    AccessList(items)
}

fn versioned_hash(rng: &mut Gen) -> B256 {
    let mut hash = rng.bytes(32);
    hash[0] = 0x01;
    B256::from_slice(&hash)
}

fn add_eip7702_authority(accounts: &mut Vec<CaseAccount>) {
    let Ok(authority) = fixed_eip7702_auth().recover_authority() else {
        return;
    };
    if accounts.iter().any(|account| account.address == authority) {
        return;
    }
    accounts.push(CaseAccount {
        address: authority,
        balance: U256::ZERO,
        nonce: 1,
        code: Bytes::new(),
        storage: BTreeMap::new(),
    });
}

pub(crate) fn fixed_eip7702_auth() -> SignedAuthorization {
    let auth = Authorization {
        chain_id: U256::from(1),
        address: Address::left_padding_from(&[6]),
        nonce: 1,
    };
    let signature = Signature::from_str(
        "48b55bfa915ac795c431978d8a6a992b628d557da5ff759b307d495a36649353efffd310ac743f371de3b9f7f9cb56c0b28ad43601b4ab949f53faa07bd2c8041b",
    )
    .expect("hard-coded EIP-7702 authorization signature must be valid");
    auth.into_signed(signature)
}

impl CaseTx {
    fn generate(
        rng: &mut Gen,
        spec: SpecId,
        accounts: &[CaseAccount],
        input_len: usize,
        nonce: u64,
    ) -> Self {
        let kind = TxKindCase::generate(rng, spec);
        Self {
            kind,
            caller: CALLER,
            target: TARGET,
            gas_limit: if kind == TxKindCase::Eip7702 {
                rng.pick(&[100_000, 250_000, 1_000_000])
            } else {
                rng.pick(&[60_000, 80_000, 100_000, 250_000, 1_000_000])
            },
            gas_price: 1,
            value: if rng.one_in(8) { rng.small_word(10) } else { U256::ZERO },
            input: rng.bytes(input_len).into(),
            nonce,
            access_list: generate_access_list(rng, accounts),
            blob_hashes: vec![versioned_hash(rng)],
        }
    }

    pub(crate) fn evm2(&self) -> RecoveredTxEnvelope {
        match self.kind {
            TxKindCase::Legacy => RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
                TxLegacy {
                    nonce: self.nonce,
                    gas_price: self.gas_price,
                    gas_limit: self.gas_limit,
                    to: TxKind::Call(self.target),
                    value: self.value,
                    input: self.input.clone(),
                    chain_id: None,
                },
                self.caller,
            )),
            TxKindCase::Eip2930 => RecoveredTxEnvelope::Eip2930(Recovered::new_unchecked(
                TxEip2930 {
                    chain_id: 1,
                    nonce: self.nonce,
                    gas_price: self.gas_price,
                    gas_limit: self.gas_limit,
                    to: TxKind::Call(self.target),
                    value: self.value,
                    access_list: self.access_list.clone(),
                    input: self.input.clone(),
                },
                self.caller,
            )),
            TxKindCase::Eip1559 => RecoveredTxEnvelope::Eip1559(Recovered::new_unchecked(
                TxEip1559 {
                    chain_id: 1,
                    nonce: self.nonce,
                    gas_limit: self.gas_limit,
                    max_fee_per_gas: self.gas_price,
                    max_priority_fee_per_gas: 0,
                    to: TxKind::Call(self.target),
                    value: self.value,
                    access_list: self.access_list.clone(),
                    input: self.input.clone(),
                },
                self.caller,
            )),
            TxKindCase::Eip4844 => RecoveredTxEnvelope::Eip4844(Recovered::new_unchecked(
                TxEip4844Variant::TxEip4844(TxEip4844 {
                    chain_id: 1,
                    nonce: self.nonce,
                    gas_limit: self.gas_limit,
                    max_fee_per_gas: self.gas_price,
                    max_priority_fee_per_gas: 0,
                    to: self.target,
                    value: self.value,
                    access_list: self.access_list.clone(),
                    blob_versioned_hashes: self.blob_hashes.clone(),
                    max_fee_per_blob_gas: 1,
                    input: self.input.clone(),
                }),
                self.caller,
            )),
            TxKindCase::Eip7702 => RecoveredTxEnvelope::Eip7702(Recovered::new_unchecked(
                TxEip7702 {
                    chain_id: 1,
                    nonce: self.nonce,
                    gas_limit: self.gas_limit,
                    max_fee_per_gas: self.gas_price,
                    max_priority_fee_per_gas: 0,
                    to: self.target,
                    value: self.value,
                    access_list: self.access_list.clone(),
                    authorization_list: vec![fixed_eip7702_auth()],
                    input: self.input.clone(),
                },
                self.caller,
            )),
        }
    }

    pub(crate) fn revm(&self) -> RevmTxEnv {
        RevmTxEnv {
            tx_type: match self.kind {
                TxKindCase::Legacy => 0,
                TxKindCase::Eip2930 => 1,
                TxKindCase::Eip1559 => 2,
                TxKindCase::Eip4844 => 3,
                TxKindCase::Eip7702 => 4,
            },
            caller: self.caller,
            gas_limit: self.gas_limit,
            gas_price: self.gas_price,
            kind: RevmTxKind::Call(self.target),
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: match self.kind {
                TxKindCase::Legacy => None,
                TxKindCase::Eip2930
                | TxKindCase::Eip1559
                | TxKindCase::Eip4844
                | TxKindCase::Eip7702 => Some(1),
            },
            access_list: self.access_list.clone(),
            gas_priority_fee: match self.kind {
                TxKindCase::Eip1559 | TxKindCase::Eip4844 | TxKindCase::Eip7702 => Some(0),
                TxKindCase::Legacy | TxKindCase::Eip2930 => None,
            },
            blob_hashes: self.blob_hashes.clone(),
            max_fee_per_blob_gas: if self.kind == TxKindCase::Eip4844 { 1 } else { 0 },
            ..RevmTxEnv::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaseAccount {
    pub(crate) address: Address,
    pub(crate) balance: U256,
    pub(crate) nonce: u64,
    pub(crate) code: Bytes,
    pub(crate) storage: BTreeMap<U256, U256>,
}

mod spec_serde {
    use super::SpecId;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S>(spec: &SpecId, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(name(*spec))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<SpecId, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        from_name(&name).ok_or_else(|| de::Error::custom(format!("unknown spec id {name:?}")))
    }

    const fn name(spec: SpecId) -> &'static str {
        match spec {
            SpecId::FRONTIER => "FRONTIER",
            SpecId::HOMESTEAD => "HOMESTEAD",
            SpecId::TANGERINE => "TANGERINE",
            SpecId::SPURIOUS_DRAGON => "SPURIOUS_DRAGON",
            SpecId::BYZANTIUM => "BYZANTIUM",
            SpecId::ISTANBUL => "ISTANBUL",
            SpecId::BERLIN => "BERLIN",
            SpecId::LONDON => "LONDON",
            SpecId::SHANGHAI => "SHANGHAI",
            SpecId::CANCUN => "CANCUN",
            SpecId::PRAGUE => "PRAGUE",
            _ => "CANCUN",
        }
    }

    fn from_name(name: &str) -> Option<SpecId> {
        match name {
            "FRONTIER" => Some(SpecId::FRONTIER),
            "HOMESTEAD" => Some(SpecId::HOMESTEAD),
            "TANGERINE" => Some(SpecId::TANGERINE),
            "SPURIOUS_DRAGON" => Some(SpecId::SPURIOUS_DRAGON),
            "BYZANTIUM" => Some(SpecId::BYZANTIUM),
            "ISTANBUL" => Some(SpecId::ISTANBUL),
            "BERLIN" => Some(SpecId::BERLIN),
            "LONDON" => Some(SpecId::LONDON),
            "SHANGHAI" => Some(SpecId::SHANGHAI),
            "CANCUN" => Some(SpecId::CANCUN),
            "PRAGUE" => Some(SpecId::PRAGUE),
            _ => None,
        }
    }
}
