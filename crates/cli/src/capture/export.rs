use super::{
    error::CaptureError,
    model::{CapturedBlock, CapturedCase, CapturedInput, State as CapturedState},
};
use crate::ethereum::mainnet_spec_for_header;
use alloy_consensus::{
    EthereumTxEnvelope, Header, TxEip4844,
    transaction::{Transaction as _, to_eip155_value},
};
use alloy_eips::{eip2930::AccessList, eip7702::SignedAuthorization, eip7840::BlobParams};
use alloy_hardforks::EthereumHardfork;
use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use evm2::SpecId;
use evm2_eest::{
    AccessListItem, TestAuthorization,
    blockchaintest::{
        Account, BlobFork, BlobScheduleEntry, Block, BlockHash as EestBlockHash, BlockHeader,
        BlockchainTest, BlockchainTestCase, ChainConfig, ForkSpec, SealEngine, State as EestState,
        Transaction, Withdrawal,
    },
};
use serde_json::json;
use std::collections::BTreeMap;

pub(super) fn suite(capture: &CapturedCase) -> Result<BlockchainTest, CaptureError> {
    let inputs = captured_blocks(capture)?;
    let blob_fork = blob_fork(inputs)?;
    let network = network(capture, blob_fork)?;
    let decoded_blocks =
        inputs.iter().map(export_block).collect::<Result<Vec<_>, CaptureError>>()?;
    let first = decoded_blocks.first().expect("blocks is not empty");
    let last = decoded_blocks.last().expect("blocks is not empty");
    let first_header = first.block_header.as_ref().expect("exported blocks include headers");
    let last_header = last.block_header.as_ref().expect("exported blocks include headers");
    let name = format!("mainnet_{}_{}", first_header.number, last_header.number);
    let genesis_block_header = export_header(&capture.parent_header);
    let lastblockhash = last_header.hash;

    let mut cases = BTreeMap::new();
    cases.insert(
        name,
        BlockchainTestCase {
            genesis_block_header,
            genesis_rlp: None,
            blocks: decoded_blocks,
            post_state: capture.post_state.as_ref().map(|state| export_state(capture, state).0),
            pre: export_state(capture, &capture.pre_state),
            block_hashes: export_block_hashes(capture),
            lastblockhash,
            network,
            seal_engine: SealEngine::NoProof,
            config: Some(ChainConfig {
                chainid: U256::ONE,
                blob_schedule: mainnet_blob_schedule(blob_fork),
            }),
        },
    );
    Ok(BlockchainTest(cases))
}

fn export_block_hashes(capture: &CapturedCase) -> Vec<EestBlockHash> {
    capture
        .pre_state
        .block_hashes
        .iter()
        .map(|block_hash| EestBlockHash {
            number: U256::from(block_hash.number),
            hash: block_hash.hash,
        })
        .collect()
}

fn captured_blocks(capture: &CapturedCase) -> Result<&[CapturedBlock], CaptureError> {
    let blocks = match &capture.input {
        CapturedInput::Block(block) => std::slice::from_ref(block.as_ref()),
        CapturedInput::Blocks(blocks) => blocks.blocks.as_slice(),
    };
    if blocks.is_empty() {
        return Err(CaptureError::EmptyCapture);
    }
    Ok(blocks)
}

/// Returns the mainnet fork that sets the blob parameters of every block, or `None` before Cancun.
fn blob_fork(blocks: &[CapturedBlock]) -> Result<Option<BlobFork>, CaptureError> {
    let mut forks = blocks.iter().map(|input| mainnet_blob_fork(&input.block.header));
    let first = forks.next().expect("blocks is not empty")?;
    for fork in forks {
        if fork? != first {
            return Err(CaptureError::MultipleSpecs);
        }
    }
    Ok(first)
}

fn mainnet_blob_fork(header: &Header) -> Result<Option<BlobFork>, CaptureError> {
    let spec = mainnet_spec_for_header(header);
    if !spec.enables(SpecId::CANCUN) {
        return Ok(None);
    }
    let hardfork = EthereumHardfork::from_chain_and_timestamp(
        alloy_chains::Chain::mainnet(),
        header.timestamp,
    );
    match hardfork {
        Some(EthereumHardfork::Cancun) => Ok(Some(BlobFork::Cancun)),
        Some(EthereumHardfork::Prague) => Ok(Some(BlobFork::Prague)),
        Some(EthereumHardfork::Osaka) => Ok(Some(BlobFork::Osaka)),
        Some(EthereumHardfork::Bpo1) => Ok(Some(BlobFork::Bpo1)),
        Some(EthereumHardfork::Bpo2) => Ok(Some(BlobFork::Bpo2)),
        Some(EthereumHardfork::Amsterdam) => Ok(Some(BlobFork::Amsterdam)),
        _ => Err(CaptureError::UnsupportedSpec(spec as u32)),
    }
}

/// Returns the mainnet blob parameters of every fork up to `last`, or none before Cancun.
fn mainnet_blob_schedule(last: Option<BlobFork>) -> BTreeMap<BlobFork, BlobScheduleEntry> {
    [
        (BlobFork::Cancun, BlobParams::cancun()),
        (BlobFork::Prague, BlobParams::prague()),
        (BlobFork::Osaka, BlobParams::osaka()),
        (BlobFork::Bpo1, BlobParams::bpo1()),
        (BlobFork::Bpo2, BlobParams::bpo2()),
        (BlobFork::Amsterdam, BlobParams::bpo2()),
    ]
    .into_iter()
    .filter(|(fork, _)| Some(*fork) <= last)
    .map(|(fork, params)| (fork, params.into()))
    .collect()
}

fn network(capture: &CapturedCase, blob_fork: Option<BlobFork>) -> Result<ForkSpec, CaptureError> {
    let first = capture.versions.versions.first().ok_or(CaptureError::EmptyCapturedVersions)?;
    if capture.versions.versions.iter().any(|version| version.spec_id != first.spec_id) {
        return Err(CaptureError::MultipleSpecs);
    }
    let spec =
        SpecId::try_from_u32(first.spec_id).ok_or(CaptureError::UnsupportedSpec(first.spec_id))?;
    match spec {
        SpecId::FRONTIER => Ok(ForkSpec::Frontier),
        SpecId::HOMESTEAD => Ok(ForkSpec::Homestead),
        SpecId::TANGERINE => Ok(ForkSpec::EIP150),
        SpecId::SPURIOUS_DRAGON => Ok(ForkSpec::EIP158),
        SpecId::BYZANTIUM => Ok(ForkSpec::Byzantium),
        SpecId::PETERSBURG => Ok(ForkSpec::ConstantinopleFix),
        SpecId::ISTANBUL => Ok(ForkSpec::Istanbul),
        SpecId::BERLIN => Ok(ForkSpec::Berlin),
        SpecId::LONDON => Ok(ForkSpec::London),
        SpecId::MERGE => Ok(ForkSpec::Paris),
        SpecId::SHANGHAI => Ok(ForkSpec::Shanghai),
        SpecId::CANCUN => Ok(ForkSpec::Cancun),
        SpecId::PRAGUE => Ok(ForkSpec::Prague),
        SpecId::OSAKA => Ok(match blob_fork {
            Some(BlobFork::Bpo1) => ForkSpec::BPO1,
            Some(BlobFork::Bpo2) => ForkSpec::BPO2,
            _ => ForkSpec::Osaka,
        }),
        SpecId::AMSTERDAM => Ok(ForkSpec::Amsterdam),
        _ => Err(CaptureError::UnsupportedSpec(first.spec_id)),
    }
}

fn export_state(capture: &CapturedCase, state: &CapturedState) -> EestState {
    let codes = capture
        .code_table
        .codes
        .iter()
        .map(|code| (code.code_hash, code.bytecode.clone()))
        .collect::<BTreeMap<_, _>>();
    let accounts = state
        .accounts
        .iter()
        .map(|account| {
            let code = codes.get(&account.code_hash).cloned().unwrap_or_default();
            let storage = account
                .storage
                .iter()
                .map(|entry| (U256::from_be_bytes(entry.slot.0), entry.value))
                .collect();
            (
                account.address,
                Account {
                    balance: account.balance,
                    code,
                    nonce: U256::from(account.nonce),
                    storage,
                },
            )
        })
        .collect();
    EestState(accounts)
}

fn export_block(input: &CapturedBlock) -> Result<Block, CaptureError> {
    let block = &input.block;
    Ok(Block {
        block_header: Some(export_header(&block.header)),
        rlp: Bytes::new(),
        expect_exception: None,
        transactions: Some(
            block
                .body
                .transactions
                .iter()
                .zip(input.transactions.iter())
                .map(|(tx, input_tx)| export_transaction(tx, input_tx.signer))
                .collect(),
        ),
        uncle_headers: Some(block.body.ommers.iter().map(export_header).collect()),
        withdrawals: Some(
            block
                .body
                .withdrawals
                .as_ref()
                .map(|withdrawals| {
                    withdrawals
                        .iter()
                        .map(|withdrawal| Withdrawal {
                            index: U256::from(withdrawal.index),
                            validator_index: U256::from(withdrawal.validator_index),
                            address: withdrawal.address,
                            amount: U256::from(withdrawal.amount),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        block_access_list: None,
        rlp_decoded: None,
    })
}

fn export_header(header: &Header) -> BlockHeader {
    BlockHeader {
        bloom: Bytes::copy_from_slice(header.logs_bloom.as_slice()),
        coinbase: header.beneficiary,
        difficulty: header.difficulty,
        extra_data: header.extra_data.clone(),
        gas_limit: U256::from(header.gas_limit),
        gas_used: U256::from(header.gas_used),
        hash: header.hash_slow(),
        mix_hash: header.mix_hash,
        nonce: FixedBytes::from(header.nonce.0),
        number: U256::from(header.number),
        parent_hash: header.parent_hash,
        receipt_trie: header.receipts_root,
        state_root: header.state_root,
        timestamp: U256::from(header.timestamp),
        transactions_trie: header.transactions_root,
        uncle_hash: header.ommers_hash,
        base_fee_per_gas: header.base_fee_per_gas.map(U256::from),
        withdrawals_root: header.withdrawals_root,
        blob_gas_used: header.blob_gas_used.map(U256::from),
        excess_blob_gas: header.excess_blob_gas.map(U256::from),
        parent_beacon_block_root: header.parent_beacon_block_root,
        requests_hash: header.requests_hash,
        target_blobs_per_block: None,
        slot_number: header.slot_number.map(U256::from),
        block_access_list_hash: header.block_access_list_hash,
    }
}

fn export_transaction(tx: &EthereumTxEnvelope<TxEip4844>, signer: Address) -> Transaction {
    let signature = tx.signature();
    let tx_type = tx_type(tx);
    Transaction {
        transaction_type: Some(U256::from(tx_type)),
        sender: Some(signer),
        data: tx.input().clone(),
        gas_limit: U256::from(tx.gas_limit()),
        gas_price: tx.gas_price().map(U256::from),
        nonce: U256::from(tx.nonce()),
        r: signature.r(),
        s: signature.s(),
        v: U256::from(signature_v(tx_type, signature.v(), tx.chain_id())),
        value: tx.value(),
        to: tx.kind().into_to(),
        chain_id: tx.chain_id().map(U256::from),
        access_list: tx.access_list().map(export_access_list),
        max_fee_per_gas: tx.tx_type().is_dynamic_fee().then(|| U256::from(tx.max_fee_per_gas())),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas().map(U256::from),
        blob_versioned_hashes: tx.blob_versioned_hashes().unwrap_or_default().to_vec(),
        max_fee_per_blob_gas: tx.max_fee_per_blob_gas().map(U256::from),
        authorization_list: tx.authorization_list().map(export_authorization_list),
        hash: Some(*tx.tx_hash()),
    }
}

fn signature_v(tx_type: u8, y_parity: bool, chain_id: Option<u64>) -> u128 {
    if tx_type == 0 { to_eip155_value(y_parity, chain_id) } else { u128::from(y_parity as u8) }
}

const fn tx_type(tx: &EthereumTxEnvelope<TxEip4844>) -> u8 {
    match tx {
        EthereumTxEnvelope::Legacy(_) => 0,
        EthereumTxEnvelope::Eip2930(_) => 1,
        EthereumTxEnvelope::Eip1559(_) => 2,
        EthereumTxEnvelope::Eip4844(_) => 3,
        EthereumTxEnvelope::Eip7702(_) => 4,
    }
}

fn export_access_list(access_list: &AccessList) -> Vec<AccessListItem> {
    access_list
        .0
        .iter()
        .map(|item| AccessListItem {
            address: item.address,
            storage_keys: item.storage_keys.clone(),
        })
        .collect()
}

fn export_authorization_list(authorizations: &[SignedAuthorization]) -> Vec<TestAuthorization> {
    authorizations
        .iter()
        .map(|authorization| TestAuthorization {
            value: json!({
                "chainId": *authorization.inner().chain_id(),
                "address": *authorization.inner().address(),
                "nonce": U256::from(authorization.inner().nonce()),
                "yParity": U256::from(authorization.y_parity()),
                "r": authorization.r(),
                "s": authorization.s(),
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_fork_follows_mainnet_activation() {
        let fork =
            |timestamp| mainnet_blob_fork(&Header { timestamp, ..Default::default() }).unwrap();
        assert_eq!(fork(1_710_338_134), None);
        assert_eq!(fork(1_710_338_135), Some(BlobFork::Cancun));
        assert_eq!(fork(1_765_290_070), Some(BlobFork::Osaka));
        assert_eq!(fork(1_765_290_071), Some(BlobFork::Bpo1));
        assert_eq!(fork(1_767_747_671), Some(BlobFork::Bpo2));
    }

    #[test]
    fn blob_schedule_lists_every_fork_up_to_the_last() {
        assert!(mainnet_blob_schedule(None).is_empty());
        let schedule = mainnet_blob_schedule(Some(BlobFork::Bpo1));
        assert_eq!(
            schedule.keys().copied().collect::<Vec<_>>(),
            [BlobFork::Cancun, BlobFork::Prague, BlobFork::Osaka, BlobFork::Bpo1]
        );
        assert_eq!(schedule[&BlobFork::Bpo1].params().update_fraction, 8_346_193);
    }
}
