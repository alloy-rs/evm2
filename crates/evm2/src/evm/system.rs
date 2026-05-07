//! System transaction execution.

use super::{Evm, TxResult};
use crate::{EvmTypes, env::TxEnv, ethereum::initial_message, interpreter::Host};
use alloc::vec::Vec;
use alloy_primitives::{Address, Bytes, TxKind, U256, address};

/// Caller address used by execution-layer system calls.
pub const SYSTEM_ADDRESS: Address = address!("0xfffffffffffffffffffffffffffffffffffffffe");

/// Gas limit used by execution-layer system calls.
pub const SYSTEM_CALL_GAS_LIMIT: u64 = 30_000_000;

/// EIP-4788 beacon roots system contract address.
pub const BEACON_ROOTS_ADDRESS: Address = address!("0x000F3df6D732807Ef1319fB7B8bB8522d0Beac02");

/// EIP-2935 historical block hashes system contract address.
pub const HISTORY_STORAGE_ADDRESS: Address = address!("0x0000F90827F1C53a10cb7A02335B175320002935");

/// EIP-7002 withdrawal request system contract address.
pub const WITHDRAWAL_REQUEST_ADDRESS: Address =
    address!("0x00000961Ef480Eb55e80D19ad83579A64c007002");

/// EIP-7251 consolidation request system contract address.
pub const CONSOLIDATION_REQUEST_ADDRESS: Address =
    address!("0x0000BBdDc7CE488642fb579F8B00f3a590007251");

impl<T: EvmTypes<Host = Self>> Evm<T> {
    /// Executes a system call from [`SYSTEM_ADDRESS`] to `system_contract_address`.
    #[inline]
    pub fn system_call(&mut self, system_contract_address: Address, data: Bytes) -> TxResult {
        self.system_call_with_caller(SYSTEM_ADDRESS, system_contract_address, data)
    }

    /// Executes a system call from `caller` to `system_contract_address`.
    ///
    /// System calls bypass normal transaction validation, nonce updates, fee charging, gas refunds,
    /// and beneficiary rewards. They execute a top-level `CALL` with zero value and
    /// [`SYSTEM_CALL_GAS_LIMIT`] gas, then finalize and return the produced state changes.
    pub fn system_call_with_caller(
        &mut self,
        caller: Address,
        system_contract_address: Address,
        data: Bytes,
    ) -> TxResult {
        self.state.warm_account_non_revertible(system_contract_address);
        let tx_env = TxEnv {
            origin: caller,
            gas_price: U256::ZERO,
            chain_id: U256::from(self.version().chain_id),
            blob_hashes: Vec::new(),
        };
        let (bytecode, message) = initial_message(
            self,
            caller,
            0,
            TxKind::Call(system_contract_address),
            &data,
            U256::ZERO,
            SYSTEM_CALL_GAS_LIMIT,
        );
        let result = Host::execute_message(self, &tx_env, bytecode, &message, false);
        let gas_spent = SYSTEM_CALL_GAS_LIMIT.saturating_sub(result.gas_remaining);
        let gas_refunded = if result.stop.is_success() && result.gas_refunded > 0 {
            result.gas_refunded as u64
        } else {
            0
        };
        let gas_used = gas_spent.saturating_sub(gas_refunded);
        let mut result = TxResult {
            status: result.stop.is_success(),
            gas_used,
            stop: result.stop,
            output: result.output,
            ..TxResult::default()
        };

        self.state.finalize_transaction(self.spec_id());
        result.state_changes = self.state.build_state_changes();
        self.state.commit_transaction_overlay();
        self.state.clear_transaction_state();
        result
    }
}
