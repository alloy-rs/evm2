//! Result, post-state and trace output in the format of `evm t8n`.

use super::{Args, BLOCK_HASH, Error, Inputs};
use alloy_consensus::{ReceiptEnvelope, Transaction, TxEnvelope, TxReceipt, Typed2718};
use alloy_eips::eip7685::Requests;
use alloy_primitives::{Address, B256, Bloom, Bytes, U256};
use evm2::evm::InMemoryDB;
use serde_json::{Map, Value, json};
use std::{io::Write, path::Path};

/// The trace of one executed transaction.
#[derive(Clone, Debug)]
pub struct TxTrace {
    /// Index among the executed transactions.
    pub index: usize,
    /// Transaction hash.
    pub hash: B256,
    /// File extension: `jsonl` for EIP-3155 lines, `json` for a tracer result.
    pub extension: &'static str,
    /// File contents.
    pub content: String,
}

impl TxTrace {
    /// Returns the file name `evm t8n` gives the trace.
    pub fn file_name(&self) -> String {
        format!("trace-{}-{}.{}", self.index, self.hash, self.extension)
    }
}

/// What a receipt takes from the block and the transaction besides the receipt itself.
#[derive(Clone, Copy, Debug)]
pub(super) struct ReceiptContext {
    pub(super) tx_index: usize,
    pub(super) contract_address: Address,
    pub(super) gas_used: u64,
    pub(super) block_number: u64,
    pub(super) block_timestamp: u64,
    pub(super) first_log_index: usize,
    pub(super) blob_base_fee: U256,
}

/// What `evm2 t8n` produces.
#[derive(Debug, Default)]
pub struct Output {
    /// Contents of `result.json`.
    pub result: Value,
    /// Contents of the post-state `alloc.json`.
    pub alloc: Value,
    /// RLP list of the executed transactions, as `--output.body` writes it.
    pub body: Bytes,
    /// One trace per executed transaction, when tracing is enabled.
    pub traces: Vec<TxTrace>,
    rejected: Vec<Value>,
    receipts: Vec<Value>,
}

impl Output {
    pub(super) fn reject(&mut self, index: usize, error: String) {
        self.rejected.push(json!({ "index": index, "error": error }));
    }

    pub(super) fn add_receipt(
        &mut self,
        receipt: &ReceiptEnvelope,
        tx: &TxEnvelope,
        context: &ReceiptContext,
    ) {
        let ReceiptContext {
            tx_index,
            contract_address,
            gas_used,
            block_number,
            block_timestamp,
            first_log_index,
            blob_base_fee,
        } = *context;
        let logs = receipt
            .logs()
            .iter()
            .enumerate()
            .map(|(offset, log)| {
                json!({
                    "address": log.address,
                    "topics": log.topics(),
                    "data": log.data.data,
                    "blockNumber": quantity(block_number),
                    "transactionHash": tx.tx_hash(),
                    "transactionIndex": quantity(tx_index as u64),
                    "blockHash": BLOCK_HASH,
                    "blockTimestamp": quantity(block_timestamp),
                    "logIndex": quantity((first_log_index + offset) as u64),
                    "removed": false,
                })
            })
            .collect::<Vec<_>>();
        let (root, status) = match receipt.status_or_post_state() {
            alloy_consensus::Eip658Value::Eip658(status) => (json!("0x"), quantity(status.into())),
            alloy_consensus::Eip658Value::PostState(root) => (json!(root), quantity(0)),
        };
        let mut value = json!({
            "root": root,
            "status": status,
            "cumulativeGasUsed": quantity(receipt.cumulative_gas_used()),
            "logsBloom": receipt.logs_bloom(),
            "logs": logs,
            "transactionHash": tx.tx_hash(),
            "contractAddress": contract_address,
            "gasUsed": quantity(gas_used),
            "effectiveGasPrice": null,
            "blockHash": BLOCK_HASH,
            "blockNumber": quantity(block_number),
            "transactionIndex": quantity(tx_index as u64),
        });
        if tx.ty() != 0 {
            value["type"] = quantity(tx.ty().into());
        }
        if tx.is_eip4844() {
            value["blobGasUsed"] = quantity(tx.blob_gas_used().unwrap_or_default());
            value["blobGasPrice"] = json!(blob_base_fee);
        }
        self.receipts.push(value);
    }

    #[expect(clippy::too_many_arguments)]
    pub(super) fn finish(
        &mut self,
        inputs: &Inputs,
        post: &InMemoryDB,
        blob_gas_used: u64,
        gas_used: u64,
        tx_root: B256,
        receipts_root: B256,
        logs_bloom: Bloom,
        logs_hash: B256,
        withdrawals_root: Option<B256>,
        requests: Option<Requests>,
        block_access_list: Option<(B256, Bytes)>,
    ) {
        let env = &inputs.env;
        let mut result = Map::new();
        result.insert("stateRoot".into(), json!(evm2_eest::state_root_from_database(post)));
        result.insert("txRoot".into(), json!(tx_root));
        result.insert("receiptsRoot".into(), json!(receipts_root));
        result.insert("logsHash".into(), json!(logs_hash));
        result.insert("logsBloom".into(), json!(logs_bloom));
        result.insert("receipts".into(), Value::Array(std::mem::take(&mut self.receipts)));
        if !self.rejected.is_empty() {
            result.insert("rejected".into(), Value::Array(std::mem::take(&mut self.rejected)));
        }
        result.insert(
            "currentDifficulty".into(),
            env.current_difficulty
                .filter(|_| env.current_random.is_none())
                .map_or(Value::Null, |difficulty| json!(difficulty)),
        );
        result.insert("gasUsed".into(), quantity(gas_used));
        if let Some(base_fee) = env.current_base_fee {
            result.insert("currentBaseFee".into(), json!(base_fee));
        }
        if let Some(root) = withdrawals_root {
            result.insert("withdrawalsRoot".into(), json!(root));
        }
        if let Some(excess) = env.current_excess_blob_gas {
            result.insert("currentExcessBlobGas".into(), json!(excess));
            result.insert("blobGasUsed".into(), quantity(blob_gas_used));
        }
        match requests {
            Some(requests) => {
                result.insert("requestsHash".into(), json!(requests.requests_hash()));
                result.insert("requests".into(), json!(requests.take()));
            }
            None => {
                result.insert("requests".into(), Value::Null);
            }
        }
        if let Some((hash, list)) = block_access_list {
            result.insert("blockAccessListHash".into(), json!(hash));
            result.insert("blockAccessList".into(), json!(list));
        }
        self.result = Value::Object(result);
        self.alloc = dump_alloc(post);
    }

    /// Writes the result, alloc, body and traces as `evm t8n` does.
    pub(super) fn write(&self, args: &Args) -> Result<(), Error> {
        let base = &args.output_basedir;
        if !base.as_os_str().is_empty() {
            std::fs::create_dir_all(base)
                .map_err(|source| Error::Write { path: base.clone(), source })?;
        }
        write_to(base, &args.output_result, &to_pretty(&self.result))?;
        write_to(base, &args.output_alloc, &self.alloc.to_string())?;
        if let Some(body) = &args.output_body {
            write_to(base, body, &json!(self.body).to_string())?;
        }
        for trace in &self.traces {
            write_to(base, &trace.file_name(), &trace.content)?;
        }
        Ok(())
    }
}

fn to_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON values serialize")
}

fn write_to(base: &Path, name: &str, content: &str) -> Result<(), Error> {
    match name {
        "stdout" => write_all(std::io::stdout(), content, Path::new("stdout")),
        "stderr" => write_all(std::io::stderr(), content, Path::new("stderr")),
        name => {
            let path = base.join(name);
            std::fs::write(&path, content).map_err(|source| Error::Write { path, source })
        }
    }
}

fn write_all(mut out: impl Write, content: &str, path: &Path) -> Result<(), Error> {
    writeln!(out, "{content}").map_err(|source| Error::Write { path: path.to_path_buf(), source })
}

/// Formats a number as a minimal `0x` hex quantity.
pub(super) fn quantity(value: u64) -> Value {
    json!(format!("{value:#x}"))
}

/// Lists the post-state accounts in the shape of `evm t8n`'s alloc dump.
fn dump_alloc(post: &InMemoryDB) -> Value {
    let mut alloc = Map::new();
    for (address, info) in &post.cache.accounts {
        let Some(info) = info else { continue };
        let mut account = Map::new();
        let code = info.code.as_ref().or_else(|| post.cache.contracts.get(&info.code_hash));
        if let Some(code) = code
            && !code.is_empty()
        {
            account.insert("code".into(), json!(code.original_bytes()));
        }
        let storage = evm2_eest::storage_for_root(post, *address);
        if !storage.is_empty() {
            let storage = storage
                .into_iter()
                .map(|(key, value)| (format!("{key:#x}"), json!(B256::from(value))))
                .collect::<Map<_, _>>();
            account.insert("storage".into(), Value::Object(storage));
        }
        account.insert("balance".into(), json!(info.balance));
        if info.nonce != 0 {
            account.insert("nonce".into(), quantity(info.nonce));
        }
        alloc.insert(format!("{address:#x}"), Value::Object(account));
    }
    Value::Object(alloc)
}
