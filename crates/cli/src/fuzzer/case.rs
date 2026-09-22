use crate::fuzzer::{
    features::FuzzFeatures,
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
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, map::HashMap};
use evm2::{
    SpecId,
    env::{BlockEnv, BlockEnvExt},
    ethereum::{RecoveredTxEnvelope, TxEnvelope},
    interpreter::op,
};
use revm::{
    context::{BlockEnv as RevmBlockEnv, TxEnv as RevmTxEnv},
    primitives::TxKind as RevmTxKind,
};
use secp256k1::{Message, SecretKey, ecdsa::RecoverableSignature};
use serde::{Deserialize, Serialize, Serializer};
use std::{fmt, sync::OnceLock};

pub(crate) const CALLER: Address = Address::new([0x10; 20]);
pub(crate) const TARGET: Address = Address::new([0x20; 20]);
pub(crate) const BENEFICIARY: Address = Address::new([0x30; 20]);
const CALLER_BALANCE: U256 = U256::from_limbs([0, 0, 1, 0]);
const EIP7702_DELEGATED_TARGET: Address =
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6]);

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EvmCase {
    #[serde(with = "spec_serde")]
    pub(crate) spec: SpecId,
    pub(crate) block: CaseBlock,
    pub(crate) tx: CaseTx,
    #[serde(default)]
    pub(crate) extra_txs: Vec<CaseTx>,
    #[serde(default)]
    pub(crate) features: FuzzFeatures,
    pub(crate) accounts: Vec<CaseAccount>,
}

impl EvmCase {
    pub(crate) fn txs(&self) -> impl Iterator<Item = &CaseTx> {
        core::iter::once(&self.tx).chain(self.extra_txs.iter())
    }
}

pub(crate) struct CaseGenerator {
    case: EvmCase,
    program: Program,
    address_pool: Vec<Address>,
    call_pool: Vec<Address>,
}

impl Default for CaseGenerator {
    fn default() -> Self {
        let mut address_pool = vec![
            CALLER,
            TARGET,
            BENEFICIARY,
            fixed_eip7702_authority(),
            EIP7702_DELEGATED_TARGET,
            Address::ZERO,
            Address::new([0xff; 20]),
        ];
        address_pool.extend((1..=10).map(Address::with_last_byte));
        let mut call_pool =
            precompile::targets().iter().map(|target| target.address()).collect::<Vec<_>>();
        call_pool.extend([CALLER, fixed_eip7702_authority(), EIP7702_DELEGATED_TARGET]);
        call_pool.extend((1..=4).map(Address::with_last_byte));
        let case = EvmCase {
            accounts: vec![
                CaseAccount { address: CALLER, balance: CALLER_BALANCE, ..Default::default() },
                CaseAccount {
                    address: TARGET,
                    balance: U256::from(1_000_000),
                    nonce: 1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        Self { case, program: Program::default(), address_pool, call_pool }
    }
}

impl CaseGenerator {
    pub(crate) fn generate(&mut self, rng: &mut Gen) -> &EvmCase {
        let Self { case, program, address_pool, call_pool } = self;
        let EvmCase { spec, block, tx, extra_txs, features, accounts } = case;
        *spec = match rng.range(13) {
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
            11 => SpecId::OSAKA,
            _ => SpecId::AMSTERDAM,
        };
        let spec = *spec;
        *block = CaseBlock::generate(rng, spec);
        let extra_accounts = rng.range_inclusive(0, 4);
        accounts.resize_with(2 + extra_accounts, CaseAccount::default);
        for (i, account) in accounts[2..].iter_mut().enumerate() {
            account.storage.clear();
            if rng.one_in(2) {
                account.storage.insert(rng.biased_word(), rng.biased_word());
            }
            account.address = Address::with_last_byte(0x40 + i as u8);
            account.balance = rng.small_word(10_000);
            account.nonce = rng.range_inclusive(0, 3) as u64;
            account.code = tiny_callee_code(rng, spec);
        }
        if address_pool.len() != 7 + 10 + extra_accounts + precompile::targets().len() + 3 {
            address_pool.truncate(7 + 10);
            call_pool.truncate(precompile::targets().len() + 3 + 4);
            for account in &accounts[2..] {
                address_pool.push(account.address);
            }
            for precompile in precompile::targets() {
                address_pool.push(precompile.address());
            }
            for account in &accounts[2..] {
                call_pool.push(account.address);
            }
            for i in 0..3 {
                let address = Address::with_last_byte(0x80 + i);
                address_pool.push(address);
                call_pool.push(address);
            }
        }
        let (program, program_features) = program.generate(rng, spec, address_pool, call_pool);
        *features = program_features;
        let target = &mut accounts[1];
        target.code = program;
        target.storage.clear();
        for _ in 0..rng.range_inclusive(0, 4) {
            target.storage.insert(rng.biased_word(), rng.biased_word());
        }
        let input_len = rng.range_inclusive(0, 64);
        tx.generate(rng, spec, accounts, input_len, 0);
        extra_txs.resize_with(rng.range_inclusive(0, 3), CaseTx::default);
        for (i, tx) in extra_txs.iter_mut().enumerate() {
            let input_len = rng.range_inclusive(0, 64);
            tx.generate(rng, spec, accounts, input_len, i as u64 + 1);
        }
        add_eip7702_accounts(
            rng,
            accounts,
            core::iter::once(&*tx).chain(extra_txs.iter()),
            features,
        );
        case
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
        BlockEnvExt {
            number: self.number,
            beneficiary: BENEFICIARY,
            timestamp: self.timestamp,
            gas_limit: U256::from(self.gas_limit),
            basefee: U256::from(self.basefee),
            ..BlockEnvExt::default()
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaseTx {
    #[serde(default)]
    pub(crate) kind: FuzzTxKind,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) enum FuzzTxKind {
    #[default]
    Legacy,
    Eip2930,
    Eip1559,
    Eip4844,
    Eip7702,
}

impl fmt::Display for FuzzTxKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Legacy => "legacy",
            Self::Eip2930 => "eip2930",
            Self::Eip1559 => "eip1559",
            Self::Eip4844 => "eip4844",
            Self::Eip7702 => "eip7702",
        })
    }
}

impl FuzzTxKind {
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
            0 if spec.enables(SpecId::PRAGUE) && rng.one_in(4) => Self::Eip7702,
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

fn generate_access_list(rng: &mut Gen, accounts: &[CaseAccount], list: &mut AccessList) {
    list.0.resize_with(rng.range_inclusive(0, 3), AccessListItem::default);
    for item in &mut list.0 {
        let account = &accounts[rng.range(accounts.len())];
        item.address = account.address;
        item.storage_keys.clear();
        for key in account.storage.keys().take(rng.range_inclusive(0, 3)) {
            item.storage_keys.push(B256::from(key.to_be_bytes::<32>()));
        }
        if item.storage_keys.is_empty() && rng.one_in(2) {
            item.storage_keys.push(B256::from(rng.biased_word().to_be_bytes::<32>()));
        }
    }
}

fn versioned_hash(rng: &mut Gen) -> B256 {
    let mut hash = rng.bytes(32);
    hash[0] = 0x01;
    B256::from_slice(&hash)
}

fn generate_eip7702_authorization_list(rng: &mut Gen, list: &mut Vec<SignedAuthorization>) {
    if rng.one_in(16) {
        return;
    }

    let len = rng.range_inclusive(1, 3);
    list.extend((0..len).map(|_| generate_eip7702_authorization(rng)));
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
    features: &mut FuzzFeatures,
) {
    let mut eip7702_txs = txs.filter(|tx| tx.kind == FuzzTxKind::Eip7702).peekable();
    if eip7702_txs.peek().is_none() {
        return;
    }

    features.insert(FuzzFeatures::EIP7702_AUTH);
    for tx in eip7702_txs {
        let auths = tx.eip7702_authorization_list();
        if auths.is_empty() {
            features.insert(FuzzFeatures::EIP7702_AUTH_EMPTY);
        }
        if auths.len() > 1 {
            features.insert(FuzzFeatures::EIP7702_AUTH_MULTI);
        }
        for auth in auths {
            if auth.y_parity() > 1 {
                features.insert(FuzzFeatures::EIP7702_AUTH_BAD_SIGNATURE);
            }
            if auth.chain_id() != &U256::ZERO && auth.chain_id() != &U256::from(1) {
                features.insert(FuzzFeatures::EIP7702_AUTH_BAD_CHAIN);
            }
            if auth.nonce() != 1 {
                features.insert(FuzzFeatures::EIP7702_AUTH_BAD_NONCE);
            }
            if auth.address() != &EIP7702_DELEGATED_TARGET {
                features.insert(FuzzFeatures::EIP7702_AUTH_ALT_DELEGATE);
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
            storage: HashMap::default(),
        },
    );

    let authority = fixed_eip7702_authority();
    accounts.retain(|account| account.address != authority);
    match rng.range(5) {
        0 => {
            features.insert(FuzzFeatures::EIP7702_AUTHORITY_MISSING);
        }
        1 => {
            features.insert(FuzzFeatures::EIP7702_AUTHORITY_VALID);
            accounts.push(eip7702_authority_account(authority, 1, Bytes::new()));
        }
        2 => {
            features.insert(FuzzFeatures::EIP7702_AUTHORITY_BAD_NONCE);
            accounts.push(eip7702_authority_account(
                authority,
                rng.pick(&[2, u64::MAX]),
                Bytes::new(),
            ));
        }
        3 => {
            features.insert(FuzzFeatures::EIP7702_AUTHORITY_REGULAR_CODE);
            accounts.push(eip7702_authority_account(authority, 1, Bytes::from_static(&[op::STOP])));
        }
        _ => {
            features.insert(FuzzFeatures::EIP7702_AUTHORITY_DELEGATED);
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

fn eip7702_authority_account(address: Address, nonce: u64, code: Bytes) -> CaseAccount {
    CaseAccount { address, balance: U256::ZERO, nonce, code, storage: HashMap::default() }
}

fn eip7702_designation(address: Address) -> Bytes {
    let mut code = vec![0xef, 0x01, 0x00];
    code.extend_from_slice(address.as_slice());
    code.into()
}

fn fixed_eip7702_authority() -> Address {
    static AUTHORITY: OnceLock<Address> = OnceLock::new();
    *AUTHORITY.get_or_init(|| {
        fixed_eip7702_auth()
            .recover_authority()
            .expect("hard-coded EIP-7702 authorization must recover an authority")
    })
}

pub(crate) fn fixed_eip7702_auth() -> SignedAuthorization {
    static AUTH: OnceLock<SignedAuthorization> = OnceLock::new();
    AUTH.get_or_init(|| {
        signed_eip7702_auth(Authorization {
            chain_id: U256::from(1),
            address: EIP7702_DELEGATED_TARGET,
            nonce: 1,
        })
    })
    .clone()
}

fn signed_eip7702_auth(auth: Authorization) -> SignedAuthorization {
    static SECRET_KEY: OnceLock<SecretKey> = OnceLock::new();
    let secret_key = SECRET_KEY.get_or_init(|| {
        SecretKey::from_secret_bytes([0x77; 32])
            .expect("hard-coded EIP-7702 signing key must be valid")
    });
    let signature = RecoverableSignature::sign_ecdsa_recoverable(
        Message::from_digest(auth.signature_hash().0),
        secret_key,
    );
    let (recovery_id, signature) = signature.serialize_compact();
    SignedAuthorization::new_unchecked(
        auth,
        recovery_id.to_u8(),
        U256::from_be_slice(&signature[..32]),
        U256::from_be_slice(&signature[32..]),
    )
}

impl CaseTx {
    fn generate(
        &mut self,
        rng: &mut Gen,
        spec: SpecId,
        accounts: &[CaseAccount],
        input_len: usize,
        nonce: u64,
    ) {
        let kind = FuzzTxKind::generate(rng, spec);
        let direct_precompile = rng.one_in(10).then(|| precompile::random_target(rng, spec));
        let creates = direct_precompile.is_none() && kind.supports_create() && rng.one_in(8);
        self.kind = kind;
        self.caller = CALLER;
        self.target = if let Some(precompile) = direct_precompile {
            precompile.address()
        } else if kind == FuzzTxKind::Eip7702 && rng.one_in(4) {
            fixed_eip7702_authority()
        } else {
            TARGET
        };
        self.creates = creates;
        self.gas_limit = if kind == FuzzTxKind::Eip7702 {
            rng.pick(&[60_000, 100_000, 250_000, 1_000_000])
        } else if creates {
            rng.pick(&[80_000, 100_000, 250_000, 1_000_000])
        } else {
            rng.pick(&[60_000, 80_000, 100_000, 250_000, 1_000_000])
        };
        self.gas_price = 1;
        self.value = if rng.one_in(8) { rng.small_word(10) } else { U256::ZERO };
        self.input = if creates {
            creation_input(rng, spec)
        } else if let Some(precompile) = direct_precompile {
            precompile::input(rng, precompile).bytes
        } else {
            rng.bytes(input_len).into()
        };
        self.nonce = nonce;
        generate_access_list(rng, accounts, &mut self.access_list);
        self.blob_hashes.clear();
        self.blob_hashes.push(versioned_hash(rng));
        if kind == FuzzTxKind::Eip7702 {
            let list = self.authorization_list.get_or_insert_default();
            list.clear();
            generate_eip7702_authorization_list(rng, list);
        } else {
            self.authorization_list = None;
        }
    }

    pub(crate) fn evm2(&self) -> RecoveredTxEnvelope {
        match self.kind {
            FuzzTxKind::Legacy => Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    nonce: self.nonce,
                    gas_price: self.gas_price,
                    gas_limit: self.gas_limit,
                    to: self.evm2_tx_kind(),
                    value: self.value,
                    input: self.input.clone(),
                    chain_id: None,
                }),
                self.caller,
            ),
            FuzzTxKind::Eip2930 => Recovered::new_unchecked(
                TxEnvelope::Eip2930(TxEip2930 {
                    chain_id: 1,
                    nonce: self.nonce,
                    gas_price: self.gas_price,
                    gas_limit: self.gas_limit,
                    to: self.evm2_tx_kind(),
                    value: self.value,
                    access_list: self.access_list.clone(),
                    input: self.input.clone(),
                }),
                self.caller,
            ),
            FuzzTxKind::Eip1559 => Recovered::new_unchecked(
                TxEnvelope::Eip1559(TxEip1559 {
                    chain_id: 1,
                    nonce: self.nonce,
                    gas_limit: self.gas_limit,
                    max_fee_per_gas: self.gas_price,
                    max_priority_fee_per_gas: 0,
                    to: self.evm2_tx_kind(),
                    value: self.value,
                    access_list: self.access_list.clone(),
                    input: self.input.clone(),
                }),
                self.caller,
            ),
            FuzzTxKind::Eip4844 => Recovered::new_unchecked(
                TxEnvelope::Eip4844(TxEip4844Variant::TxEip4844(TxEip4844 {
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
                })),
                self.caller,
            ),
            FuzzTxKind::Eip7702 => Recovered::new_unchecked(
                TxEnvelope::Eip7702(
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
                    }
                    .into(),
                ),
                self.caller,
            ),
        }
    }

    pub(crate) fn eip7702_authorization_list(&self) -> Vec<SignedAuthorization> {
        if self.kind == FuzzTxKind::Eip7702 {
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

    pub(crate) fn precompile_input_shape(&self, precompile: PrecompileTarget) -> FuzzFeatures {
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
                FuzzTxKind::Legacy => 0,
                FuzzTxKind::Eip2930 => 1,
                FuzzTxKind::Eip1559 => 2,
                FuzzTxKind::Eip4844 => 3,
                FuzzTxKind::Eip7702 => 4,
            },
            caller: self.caller,
            gas_limit: self.gas_limit,
            gas_price: self.gas_price,
            kind: self.revm_tx_kind(),
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: match self.kind {
                FuzzTxKind::Legacy => None,
                FuzzTxKind::Eip2930
                | FuzzTxKind::Eip1559
                | FuzzTxKind::Eip4844
                | FuzzTxKind::Eip7702 => Some(1),
            },
            access_list: self.access_list.clone(),
            gas_priority_fee: match self.kind {
                FuzzTxKind::Eip1559 | FuzzTxKind::Eip4844 | FuzzTxKind::Eip7702 => Some(0),
                FuzzTxKind::Legacy | FuzzTxKind::Eip2930 => None,
            },
            blob_hashes: self.blob_hashes.clone(),
            max_fee_per_blob_gas: if self.kind == FuzzTxKind::Eip4844 { 1 } else { 0 },
            ..RevmTxEnv::default()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaseAccount {
    pub(crate) address: Address,
    pub(crate) balance: U256,
    pub(crate) nonce: u64,
    pub(crate) code: Bytes,
    #[serde(serialize_with = "serialize_storage")]
    pub(crate) storage: HashMap<U256, U256>,
}

fn serialize_storage<S: Serializer>(
    storage: &HashMap<U256, U256>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut entries = storage.iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|&(key, _)| key);
    serializer.collect_map(entries)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regeneration_matches_fresh_case() {
        let mut generator = CaseGenerator::default();
        for seed in 0..512 {
            let mut rng = Gen::new(seed);
            let mut fresh_rng = Gen::new(seed);
            let mut reused = generator.generate(&mut rng).clone();
            let mut fresh = CaseGenerator::default().generate(&mut fresh_rng).clone();
            assert_eq!(rng.bytes(32), fresh_rng.bytes(32), "RNG state at seed {seed}");
            // HashMap iteration can select different storage keys, but not different counts.
            for case in [&mut reused, &mut fresh] {
                for tx in core::iter::once(&mut case.tx).chain(&mut case.extra_txs) {
                    for item in &mut tx.access_list.0 {
                        item.storage_keys.fill(B256::ZERO);
                    }
                }
            }
            assert_eq!(reused, fresh, "seed {seed}");
        }
    }

    #[test]
    fn storage_serialization_is_canonical() {
        let mut case = CaseGenerator::default().generate(&mut Gen::new(1)).clone();
        case.accounts[0].storage = (0..32).map(|key| (U256::from(key), U256::ONE)).collect();
        let json = serde_json::to_vec(&case).unwrap();
        case.accounts[0].storage = (0..32).rev().map(|key| (U256::from(key), U256::ONE)).collect();
        assert_eq!(serde_json::to_vec(&case).unwrap(), json);
        let decoded = serde_json::from_slice::<EvmCase>(&json).unwrap();
        assert_eq!(decoded, case);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), json);
    }
}
