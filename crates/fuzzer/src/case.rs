use crate::{
    precompile::{self, PrecompileTarget},
    program::Program,
    rng::Gen,
};
use alloy_consensus::{
    TxEip1559, TxEip2930, TxEip4844, TxEip7702, TxLegacy,
    transaction::{Recovered, TxEip4844Variant},
};
use alloy_eips::{
    eip2930::{AccessList, AccessListItem},
    eip7702::{Authorization, SignedAuthorization},
};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use evm2::{SpecId, env::BlockEnv, ethereum::RecoveredTxEnvelope, interpreter::op};
use revm::{
    context::{BlockEnv as RevmBlockEnv, TxEnv as RevmTxEnv},
    primitives::TxKind as RevmTxKind,
};
use secp256k1::{Message, SECP256K1, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) const CALLER: Address = Address::new([0x10; 20]);
pub(crate) const TARGET: Address = Address::new([0x20; 20]);
pub(crate) const BENEFICIARY: Address = Address::new([0x30; 20]);
const CALLER_BALANCE: U256 = U256::from_limbs([0, 0, 1, 0]);
const EIP7702_DELEGATED_TARGET: Address =
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6]);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EvmCase {
    #[serde(with = "spec_serde")]
    pub(crate) spec: SpecId,
    pub(crate) block: CaseBlock,
    pub(crate) tx: CaseTx,
    #[serde(default)]
    pub(crate) extra_txs: Vec<CaseTx>,
    #[serde(default)]
    pub(crate) features: Vec<String>,
    pub(crate) accounts: Vec<CaseAccount>,
}

impl EvmCase {
    pub(crate) fn txs(&self) -> impl Iterator<Item = &CaseTx> {
        core::iter::once(&self.tx).chain(self.extra_txs.iter())
    }

    pub(crate) fn generate(rng: &mut Gen) -> Self {
        let spec = match rng.range(12) {
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
            10 => SpecId::PRAGUE,
            // TODO: Re-enable Amsterdam once evm2's EIP-8037 state-gas accounting is aligned
            // with revm. Manual Amsterdam replay remains supported through serde parsing/mapping.
            _ => SpecId::OSAKA,
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
        let eip7702_authority = fixed_eip7702_authority();
        let mut address_pool = vec![
            CALLER,
            TARGET,
            BENEFICIARY,
            eip7702_authority,
            EIP7702_DELEGATED_TARGET,
            Address::ZERO,
            Address::new([0xff; 20]),
        ];

        for i in 1..=10 {
            address_pool.push(Address::with_last_byte(i));
        }
        for account in &extra_accounts {
            address_pool.push(account.address);
        }
        let mut call_pool = Vec::new();
        for precompile in precompile::targets() {
            address_pool.push(precompile.address());
            call_pool.push(precompile.address());
        }
        call_pool.push(CALLER);
        call_pool.push(eip7702_authority);
        call_pool.push(EIP7702_DELEGATED_TARGET);
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
        let (program, mut features) =
            Program::generate(rng, spec, &address_pool, &call_pool).into_parts();
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
        let mut extra_txs = Vec::new();
        for nonce in 1..=rng.range_inclusive(0, 3) as u64 {
            let input_len = rng.range_inclusive(0, 64);
            extra_txs.push(CaseTx::generate(rng, spec, &accounts, input_len, nonce));
        }
        add_eip7702_accounts(
            rng,
            &mut accounts,
            core::iter::once(&tx).chain(&extra_txs),
            &mut features,
        );
        features.sort();
        features.dedup();
        Self { spec, block, tx, extra_txs, features, accounts }
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

fn creation_input(rng: &mut Gen, spec: SpecId) -> Bytes {
    match rng.range(5) {
        0 => Bytes::new(),
        1 => [op::PUSH1, 0, op::PUSH1, 0, op::MSTORE8, op::PUSH1, 1, op::PUSH1, 0, op::RETURN]
            .into_iter()
            .collect::<Vec<_>>()
            .into(),
        2 => returning_callee_code(op::RETURN),
        3 if spec.enables(SpecId::BYZANTIUM) => {
            [op::PUSH1, 0, op::PUSH1, 0, op::REVERT].into_iter().collect::<Vec<_>>().into()
        }
        _ => {
            let len = rng.range_inclusive(1, 32);
            rng.bytes(len).into()
        }
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) creates: bool,
    pub(crate) gas_limit: u64,
    pub(crate) gas_price: u128,
    pub(crate) value: U256,
    pub(crate) input: Bytes,
    pub(crate) nonce: u64,
    #[serde(default)]
    pub(crate) access_list: AccessList,
    #[serde(default)]
    pub(crate) blob_hashes: Vec<B256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) authorization_list: Option<Vec<SignedAuthorization>>,
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
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Eip2930 => "eip2930",
            Self::Eip1559 => "eip1559",
            Self::Eip4844 => "eip4844",
            Self::Eip7702 => "eip7702",
        }
    }

    pub(crate) const fn is_enabled(self, spec: SpecId) -> bool {
        match self {
            Self::Legacy => true,
            Self::Eip2930 => spec.enables(SpecId::BERLIN),
            Self::Eip1559 => spec.enables(SpecId::LONDON),
            Self::Eip4844 => spec.enables(SpecId::CANCUN),
            Self::Eip7702 => spec.enables(SpecId::PRAGUE),
        }
    }

    const fn supports_create(self) -> bool {
        matches!(self, Self::Legacy | Self::Eip2930 | Self::Eip1559)
    }

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

fn generate_eip7702_authorization_list(rng: &mut Gen) -> Vec<SignedAuthorization> {
    if rng.one_in(16) {
        return Vec::new();
    }

    let len = rng.range_inclusive(1, 3);
    (0..len).map(|_| generate_eip7702_authorization(rng)).collect()
}

fn generate_eip7702_authorization(rng: &mut Gen) -> SignedAuthorization {
    match rng.range(6) {
        0..=1 => fixed_eip7702_auth(),
        2 => signed_eip7702_auth(Authorization {
            chain_id: U256::ZERO,
            address: rng.pick(&[EIP7702_DELEGATED_TARGET, TARGET, Address::ZERO]),
            nonce: 1,
        }),
        3 => signed_eip7702_auth(Authorization {
            chain_id: U256::from(1),
            address: rng.pick(&[EIP7702_DELEGATED_TARGET, TARGET, Address::with_last_byte(8)]),
            nonce: 1,
        }),
        4 => signed_eip7702_auth(Authorization {
            chain_id: rng.pick(&[U256::from(1), U256::from(2)]),
            address: EIP7702_DELEGATED_TARGET,
            nonce: rng.pick(&[0, 2, u64::MAX]),
        }),
        _ => {
            let auth = fixed_eip7702_auth();
            SignedAuthorization::new_unchecked(auth.inner().clone(), 2, auth.r(), auth.s())
        }
    }
}

fn add_eip7702_accounts<'a>(
    rng: &mut Gen,
    accounts: &mut Vec<CaseAccount>,
    txs: impl Iterator<Item = &'a CaseTx>,
    features: &mut Vec<String>,
) {
    let eip7702_txs = txs.filter(|tx| tx.kind == TxKindCase::Eip7702).collect::<Vec<_>>();
    if eip7702_txs.is_empty() {
        return;
    }

    features.push("eip7702_auth".to_string());
    for tx in eip7702_txs {
        let auths = tx.eip7702_authorization_list();
        if auths.is_empty() {
            features.push("eip7702_auth_empty".to_string());
        }
        if auths.len() > 1 {
            features.push("eip7702_auth_multi".to_string());
        }
        for auth in auths {
            if auth.y_parity() > 1 {
                features.push("eip7702_auth_bad_signature".to_string());
            }
            if auth.chain_id() != &U256::ZERO && auth.chain_id() != &U256::from(1) {
                features.push("eip7702_auth_bad_chain".to_string());
            }
            if auth.nonce() != 1 {
                features.push("eip7702_auth_bad_nonce".to_string());
            }
            if auth.address() != &EIP7702_DELEGATED_TARGET {
                features.push("eip7702_auth_alt_delegate".to_string());
            }
        }
    }
    upsert_account(
        accounts,
        CaseAccount {
            address: EIP7702_DELEGATED_TARGET,
            balance: U256::from(1_000),
            nonce: 0,
            code: tiny_callee_code(rng, SpecId::PRAGUE),
            storage: BTreeMap::new(),
        },
    );

    let authority = fixed_eip7702_authority();
    accounts.retain(|account| account.address != authority);
    match rng.range(5) {
        0 => features.push("eip7702_authority_missing".to_string()),
        1 => {
            features.push("eip7702_authority_valid".to_string());
            accounts.push(eip7702_authority_account(authority, 1, Bytes::new()));
        }
        2 => {
            features.push("eip7702_authority_bad_nonce".to_string());
            accounts.push(eip7702_authority_account(
                authority,
                rng.pick(&[2, u64::MAX]),
                Bytes::new(),
            ));
        }
        3 => {
            features.push("eip7702_authority_regular_code".to_string());
            accounts.push(eip7702_authority_account(authority, 1, Bytes::from_static(&[op::STOP])));
        }
        _ => {
            features.push("eip7702_authority_delegated".to_string());
            accounts.push(eip7702_authority_account(
                authority,
                1,
                eip7702_designation(Address::with_last_byte(7)),
            ));
        }
    }
}

fn upsert_account(accounts: &mut Vec<CaseAccount>, account: CaseAccount) {
    accounts.retain(|existing| existing.address != account.address);
    accounts.push(account);
}

const fn eip7702_authority_account(address: Address, nonce: u64, code: Bytes) -> CaseAccount {
    CaseAccount { address, balance: U256::ZERO, nonce, code, storage: BTreeMap::new() }
}

fn eip7702_designation(address: Address) -> Bytes {
    let mut code = vec![0xef, 0x01, 0x00];
    code.extend_from_slice(address.as_slice());
    code.into()
}

fn fixed_eip7702_authority() -> Address {
    fixed_eip7702_auth()
        .recover_authority()
        .expect("hard-coded EIP-7702 authorization must recover an authority")
}

pub(crate) fn fixed_eip7702_auth() -> SignedAuthorization {
    signed_eip7702_auth(Authorization {
        chain_id: U256::from(1),
        address: EIP7702_DELEGATED_TARGET,
        nonce: 1,
    })
}

fn signed_eip7702_auth(auth: Authorization) -> SignedAuthorization {
    let secret_key = SecretKey::from_byte_array([0x77; 32])
        .expect("hard-coded EIP-7702 signing key must be valid");
    let signature = SECP256K1
        .sign_ecdsa_recoverable(Message::from_digest(auth.signature_hash().0), &secret_key);
    let (recovery_id, signature) = signature.serialize_compact();
    SignedAuthorization::new_unchecked(
        auth,
        i32::from(recovery_id) as u8,
        U256::from_be_slice(&signature[..32]),
        U256::from_be_slice(&signature[32..]),
    )
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
        let direct_precompile = rng.one_in(10).then(|| precompile::random_target(rng, spec));
        let creates = direct_precompile.is_none() && kind.supports_create() && rng.one_in(8);
        Self {
            kind,
            caller: CALLER,
            target: if let Some(precompile) = direct_precompile {
                precompile.address()
            } else if kind == TxKindCase::Eip7702 && rng.one_in(4) {
                fixed_eip7702_authority()
            } else {
                TARGET
            },
            creates,
            gas_limit: if kind == TxKindCase::Eip7702 {
                rng.pick(&[60_000, 100_000, 250_000, 1_000_000])
            } else if creates {
                rng.pick(&[80_000, 100_000, 250_000, 1_000_000])
            } else {
                rng.pick(&[60_000, 80_000, 100_000, 250_000, 1_000_000])
            },
            gas_price: 1,
            value: if rng.one_in(8) { rng.small_word(10) } else { U256::ZERO },
            input: if creates {
                creation_input(rng, spec)
            } else if let Some(precompile) = direct_precompile {
                precompile::input(rng, precompile).bytes
            } else {
                rng.bytes(input_len).into()
            },
            nonce,
            access_list: generate_access_list(rng, accounts),
            blob_hashes: vec![versioned_hash(rng)],
            authorization_list: (kind == TxKindCase::Eip7702)
                .then(|| generate_eip7702_authorization_list(rng)),
        }
    }

    pub(crate) fn evm2(&self) -> RecoveredTxEnvelope {
        match self.kind {
            TxKindCase::Legacy => RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
                TxLegacy {
                    nonce: self.nonce,
                    gas_price: self.gas_price,
                    gas_limit: self.gas_limit,
                    to: self.evm2_tx_kind(),
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
                    to: self.evm2_tx_kind(),
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
                    to: self.evm2_tx_kind(),
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
                    authorization_list: self.eip7702_authorization_list(),
                    input: self.input.clone(),
                },
                self.caller,
            )),
        }
    }

    pub(crate) fn eip7702_authorization_list(&self) -> Vec<SignedAuthorization> {
        if self.kind == TxKindCase::Eip7702 {
            self.authorization_list.clone().unwrap_or_else(|| vec![fixed_eip7702_auth()])
        } else {
            Vec::new()
        }
    }

    pub(crate) const fn is_create(&self) -> bool {
        self.creates
    }

    pub(crate) fn direct_precompile(&self) -> Option<PrecompileTarget> {
        if self.creates { None } else { precompile::target_for_address(self.target) }
    }

    pub(crate) fn precompile_input_shape(&self, precompile: PrecompileTarget) -> &'static str {
        precompile::input_shape(precompile, self.input.len())
    }

    const fn evm2_tx_kind(&self) -> TxKind {
        if self.creates { TxKind::Create } else { TxKind::Call(self.target) }
    }

    const fn revm_tx_kind(&self) -> RevmTxKind {
        if self.creates { RevmTxKind::Create } else { RevmTxKind::Call(self.target) }
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
            kind: self.revm_tx_kind(),
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
            SpecId::PETERSBURG => "PETERSBURG",
            SpecId::ISTANBUL => "ISTANBUL",
            SpecId::BERLIN => "BERLIN",
            SpecId::LONDON => "LONDON",
            SpecId::MERGE => "MERGE",
            SpecId::SHANGHAI => "SHANGHAI",
            SpecId::CANCUN => "CANCUN",
            SpecId::PRAGUE => "PRAGUE",
            SpecId::OSAKA => "OSAKA",
            SpecId::AMSTERDAM => "AMSTERDAM",
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
            "PETERSBURG" => Some(SpecId::PETERSBURG),
            "ISTANBUL" => Some(SpecId::ISTANBUL),
            "BERLIN" => Some(SpecId::BERLIN),
            "LONDON" => Some(SpecId::LONDON),
            "MERGE" => Some(SpecId::MERGE),
            "SHANGHAI" => Some(SpecId::SHANGHAI),
            "CANCUN" => Some(SpecId::CANCUN),
            "PRAGUE" => Some(SpecId::PRAGUE),
            "OSAKA" => Some(SpecId::OSAKA),
            "AMSTERDAM" => Some(SpecId::AMSTERDAM),
            _ => None,
        }
    }
}
