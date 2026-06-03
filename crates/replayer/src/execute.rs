use crate::{
    error::{Error, Result},
    prepare::{PreparedBlock, PreparedTransaction},
};
use alloy_primitives::{B256, Bytes};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, TxResult, Version,
    ethereum::ethereum_tx_registry,
    evm::{BEACON_ROOTS_ADDRESS, HISTORY_STORAGE_ADDRESS},
    registry::HandlerError,
};

pub(crate) fn execute_block(prepared: PreparedBlock) -> Result<BlockExecution> {
    let PreparedBlock {
        block_number,
        block_hash,
        gas_used,
        parent_hash,
        parent_beacon_block_root,
        spec,
        block_env,
        db,
        transactions,
    } = prepared;
    let execution_config = execution_config(spec);
    let mut evm = Evm::new_with_execution_config(
        execution_config,
        spec,
        block_env,
        ethereum_tx_registry(spec),
        db,
        Precompiles::base(spec),
    );
    apply_pre_execution_system_calls(&mut evm, spec, parent_hash, parent_beacon_block_root)?;
    let mut results = Vec::with_capacity(transactions.len());

    for PreparedTransaction { tx_hash, tx } in transactions {
        let result = evm.transact(&tx);
        results.push(TxExecution { tx_hash, result });
    }

    // # Where is `apply_post_execution_system_calls`?
    //
    // Currently, we are only verifying the gas_used. This is enough for the goals of this replayer
    // since it focuses only on the evm2 behavior. Should we add the whole block output
    // verification in this replayer we will need to call `apply_post_execution_system_calls` after
    // transaction execution.

    verify_gas_used(block_number, block_hash, gas_used, &results)?;
    Ok(BlockExecution { gas_used })
}

type TxResultOrError = std::result::Result<TxResult<BaseEvmTypes>, HandlerError>;

#[derive(Debug)]
pub(crate) struct BlockExecution {
    pub(crate) gas_used: u64,
}

#[derive(Debug)]
struct TxExecution {
    tx_hash: B256,
    result: TxResultOrError,
}

fn verify_gas_used(
    block_number: u64,
    block_hash: B256,
    expected: u64,
    transactions: &[TxExecution],
) -> Result<()> {
    let mut actual = 0u128;
    for (index, tx) in transactions.iter().enumerate() {
        let result = tx.result.as_ref().map_err(|source| Error::TransactionExecution {
            block_number,
            block_hash,
            index,
            tx_hash: tx.tx_hash,
            source: Box::new(*source),
        })?;
        actual += u128::from(result.gas_used);
    }
    if actual != u128::from(expected) {
        return Err(Error::GasUsedMismatch { block_number, block_hash, actual, expected });
    }
    Ok(())
}

fn apply_pre_execution_system_calls(
    evm: &mut Evm<BaseEvmTypes>,
    spec: SpecId,
    parent_hash: B256,
    parent_beacon_block_root: Option<B256>,
) -> Result<()> {
    if spec.enables(SpecId::PRAGUE) {
        // EIP-2935 ("Serve historical block hashes from state") moves BLOCKHASH lookup data into
        // the EIP-2935 history storage system contract from Prague onward. At the start of each
        // non-genesis Prague+ block, clients make a system call from the system address to
        // HISTORY_STORAGE_ADDRESS with the parent block hash as calldata. The contract stores that
        // hash in its ring buffer so later EVM execution can serve recent BLOCKHASH queries from
        // state. This mirrors revm/alloy's pre-block blockhashes contract call; mainnet genesis is
        // naturally excluded here because it predates Prague.
        let history_result = evm
            .system_call(HISTORY_STORAGE_ADDRESS, Bytes::copy_from_slice(parent_hash.as_slice()));
        if !history_result.status {
            return Err(Error::HistoryStorageSystemCall {
                stop: format!("{:?}", history_result.stop),
            });
        }
    }

    if spec.enables(SpecId::CANCUN)
        && let Some(root) = parent_beacon_block_root
    {
        // EIP-4788 ("Beacon block root in the EVM") similarly requires a pre-block system call
        // from Cancun onward. If the execution block carries a parent beacon block root, clients
        // call the beacon roots contract with that root as calldata so contracts can later read
        // consensus-layer root history from state.
        let beacon_result =
            evm.system_call(BEACON_ROOTS_ADDRESS, Bytes::copy_from_slice(root.as_slice()));
        if !beacon_result.status {
            return Err(Error::BeaconRootsSystemCall { stop: format!("{:?}", beacon_result.stop) });
        }
    }

    Ok(())
}

fn execution_config(spec: SpecId) -> ExecutionConfig<BaseEvmTypes> {
    let version = Version::new(spec);
    ExecutionConfig::for_spec_and_version(spec, version)
}
