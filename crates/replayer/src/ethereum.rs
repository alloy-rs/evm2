use crate::{
    corpus,
    error::{Error, Result},
};
use alloy_consensus::{
    EthereumTxEnvelope, Header, TxEip4844,
    transaction::{Recovered, TxEip4844Variant},
};
use alloy_eips::eip7840::BlobParams;
use alloy_primitives::{Address, U256};
use evm2::{
    SpecId,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::RecoveredTxEnvelope,
    evm::{AccountInfo, InMemoryDB},
};
use std::collections::BTreeMap;

pub(crate) const fn mainnet_spec_for_header(header: &Header) -> SpecId {
    const HOMESTEAD_BLOCK: u64 = 1_150_000;
    const TANGERINE_BLOCK: u64 = 2_463_000;
    const SPURIOUS_DRAGON_BLOCK: u64 = 2_675_000;
    const BYZANTIUM_BLOCK: u64 = 4_370_000;
    const PETERSBURG_BLOCK: u64 = 7_280_000;
    const ISTANBUL_BLOCK: u64 = 9_069_000;
    const BERLIN_BLOCK: u64 = 12_244_000;
    const LONDON_BLOCK: u64 = 12_965_000;
    const MERGE_BLOCK: u64 = 15_537_394;
    const SHANGHAI_TIMESTAMP: u64 = 1_681_338_455;
    const CANCUN_TIMESTAMP: u64 = 1_710_338_135;
    const PRAGUE_TIMESTAMP: u64 = 1_746_612_311;
    const OSAKA_TIMESTAMP: u64 = 1_764_798_551;

    if header.timestamp >= OSAKA_TIMESTAMP {
        SpecId::OSAKA
    } else if header.timestamp >= PRAGUE_TIMESTAMP {
        SpecId::PRAGUE
    } else if header.timestamp >= CANCUN_TIMESTAMP {
        SpecId::CANCUN
    } else if header.timestamp >= SHANGHAI_TIMESTAMP {
        SpecId::SHANGHAI
    } else if header.number >= MERGE_BLOCK {
        SpecId::MERGE
    } else if header.number >= LONDON_BLOCK {
        SpecId::LONDON
    } else if header.number >= BERLIN_BLOCK {
        SpecId::BERLIN
    } else if header.number >= ISTANBUL_BLOCK {
        SpecId::ISTANBUL
    } else if header.number >= PETERSBURG_BLOCK {
        SpecId::PETERSBURG
    } else if header.number >= BYZANTIUM_BLOCK {
        SpecId::BYZANTIUM
    } else if header.number >= SPURIOUS_DRAGON_BLOCK {
        SpecId::SPURIOUS_DRAGON
    } else if header.number >= TANGERINE_BLOCK {
        SpecId::TANGERINE
    } else if header.number >= HOMESTEAD_BLOCK {
        SpecId::HOMESTEAD
    } else {
        SpecId::FRONTIER
    }
}

pub(crate) fn block_env_for_header(header: &Header, spec: SpecId) -> Result<BlockEnv> {
    let basefee = if spec.enables(SpecId::LONDON) {
        header.base_fee_per_gas.ok_or(Error::MissingBaseFee { block_number: header.number })?
    } else {
        0
    };

    let blob_basefee = if spec.enables(SpecId::CANCUN) {
        let excess_blob_gas = header
            .excess_blob_gas
            .ok_or(Error::MissingExcessBlobGas { block_number: header.number })?;
        U256::from(blob_params_for_header(header, spec).calc_blob_fee(excess_blob_gas))
    } else {
        U256::ZERO
    };

    Ok(BlockEnv {
        number: U256::from(header.number),
        beneficiary: header.beneficiary,
        timestamp: U256::from(header.timestamp),
        gas_limit: U256::from(header.gas_limit),
        basefee: U256::from(basefee),
        difficulty: header.difficulty,
        prevrandao: header.mix_hash.into(),
        blob_basefee,
        ..BlockEnv::default()
    })
}

pub(crate) fn db_from_state(state: &corpus::State) -> InMemoryDB {
    let mut db = InMemoryDB::default();
    let contracts = state
        .contracts
        .iter()
        .map(|contract| (contract.code_hash, contract.bytecode.clone()))
        .collect::<BTreeMap<_, _>>();

    for account in &state.accounts {
        let code = contracts.get(&account.code_hash).cloned().unwrap_or_default();
        let mut info = AccountInfo::default().with_code(Bytecode::new_raw(code));
        info.balance = account.balance;
        info.nonce = account.nonce;
        info.code_hash = account.code_hash;
        db.insert_account_info(&account.address, info);
    }

    for slot in &state.storage {
        db.insert_account_storage(&slot.address, &U256::from_be_bytes(slot.slot.0), &slot.value);
    }

    for block_hash in &state.block_hashes {
        db.insert_block_hash(&U256::from(block_hash.number), &block_hash.hash);
    }

    db
}

pub(crate) fn tx_from_consensus(
    signer: Address,
    tx: &EthereumTxEnvelope<TxEip4844>,
) -> RecoveredTxEnvelope {
    match tx {
        EthereumTxEnvelope::Legacy(tx) => {
            RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx.tx().clone(), signer))
        }
        EthereumTxEnvelope::Eip2930(tx) => {
            RecoveredTxEnvelope::Eip2930(Recovered::new_unchecked(tx.tx().clone(), signer))
        }
        EthereumTxEnvelope::Eip1559(tx) => {
            RecoveredTxEnvelope::Eip1559(Recovered::new_unchecked(tx.tx().clone(), signer))
        }
        EthereumTxEnvelope::Eip4844(tx) => RecoveredTxEnvelope::Eip4844(Recovered::new_unchecked(
            TxEip4844Variant::from(tx.tx().clone()),
            signer,
        )),
        EthereumTxEnvelope::Eip7702(tx) => {
            RecoveredTxEnvelope::Eip7702(Recovered::new_unchecked(tx.tx().clone(), signer))
        }
    }
}

const fn blob_params_for_header(header: &Header, spec: SpecId) -> BlobParams {
    const MAINNET_BPO1_TIMESTAMP: u64 = 1_765_290_071;
    const MAINNET_BPO2_TIMESTAMP: u64 = 1_767_747_671;

    if header.timestamp >= MAINNET_BPO2_TIMESTAMP {
        BlobParams::bpo2()
    } else if header.timestamp >= MAINNET_BPO1_TIMESTAMP {
        BlobParams::bpo1()
    } else if spec.enables(SpecId::OSAKA) {
        BlobParams::osaka()
    } else if spec.enables(SpecId::PRAGUE) {
        BlobParams::prague()
    } else {
        BlobParams::cancun()
    }
}
