//! System transaction execution.
//!
//! System calls execute bytecode already present at the target system contract address. The EVM
//! does not install or inject protocol system contract bytecode; callers that use these hooks are
//! responsible for making the target account and code available in the backing database or overlay
//! before calling [`Evm::system_call`]. Calling an address without code succeeds as an empty call
//! and produces no state changes.

#[cfg(feature = "async")]
use super::SendEvmRef;
#[cfg(feature = "async")]
use super::r#async;
use super::{Evm, ExecutedTx, TxResult};
use crate::{
    EvmTypes,
    env::TxEnv,
    ethereum::initial_message,
    interpreter::{Host, InstrStop},
};
use alloc::vec::Vec;
use alloy_primitives::{Address, Bytes, TxKind, U256, address};
#[cfg(feature = "async")]
use core::{convert::Infallible, future::Future};

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
    pub fn system_call(
        &mut self,
        system_contract_address: Address,
        data: Bytes,
    ) -> ExecutedTx<'_, T> {
        self.system_call_with_caller(SYSTEM_ADDRESS, system_contract_address, data)
    }

    /// Executes a system call from [`SYSTEM_ADDRESS`] to `system_contract_address` on an async
    /// fiber.
    ///
    /// This must be used with an async database adapter such as
    /// [`evm::async::AsyncDb`](crate::evm::async::AsyncDb) to take
    /// advantage of yielding database I/O. With a synchronous database this is mostly equivalent to
    /// running the synchronous system call on a fiber.
    ///
    /// This returns a `Send` future. Before calling it, the current erased database, precompile
    /// provider, and optional inspector must be verified with [`Evm::evm_is_send`] or
    /// [`Evm::evm_is_send_with_inspector`].
    #[cfg(feature = "async")]
    #[inline]
    pub fn system_call_async(
        &mut self,
        system_contract_address: Address,
        data: Bytes,
    ) -> impl Future<Output = r#async::AsyncResult<TxResult<T>, Infallible>> + Send + '_
    where
        T::TxResultExt: Send,
    {
        self.assert_erased_send();
        let stack = self.async_stack();
        let mut evm = SendEvmRef::new(self);
        // SAFETY: The returned future owns the exclusive `&mut self` borrow, so nothing else can
        // access the EVM stack slot until that future is dropped. The send marker checked above
        // requires all erased EVM fields to have been verified by `Evm::evm_is_send`.
        unsafe {
            r#async::on_fiber_with_stack(stack, move || {
                evm.system_call(system_contract_address, data)
            })
        }
    }

    /// Executes a system call from `caller` to `system_contract_address`.
    ///
    /// System calls bypass normal transaction validation, nonce updates, fee charging, gas refunds,
    /// and beneficiary rewards. They execute a top-level `CALL` with zero value and
    /// [`SYSTEM_CALL_GAS_LIMIT`] gas, then finalize and return an executed transaction handle.
    ///
    /// The target system contract bytecode must already be present in state. This method does not
    /// deploy protocol system contracts or synthesize their bytecode.
    pub fn system_call_with_caller(
        &mut self,
        caller: Address,
        system_contract_address: Address,
        data: Bytes,
    ) -> ExecutedTx<'_, T> {
        self.state.prewarmset_mut().warm_account(&system_contract_address);
        let tx_env = TxEnv {
            origin: caller,
            gas_price: U256::ZERO,
            chain_id: U256::from(self.version().chain_id),
            blob_hashes: Vec::new(),
            ext: T::TxEnvExt::default(),
            _non_exhaustive: (),
        };
        let Ok((bytecode, mut message)) = initial_message(
            self,
            caller,
            0,
            TxKind::Call(system_contract_address),
            &data,
            U256::ZERO,
            SYSTEM_CALL_GAS_LIMIT,
        ) else {
            self.state.clear_transaction_state();
            let stop = InstrStop::FatalExternalError;
            let outcome =
                TxResult { stop, db_error_code: self.db_error_code(), ..TxResult::default() };
            return ExecutedTx::from_result(self, outcome, false);
        };
        // System calls are not inspected.
        let inspector = self.inspector.take();
        let result = Host::execute_message(self, &tx_env, bytecode, &mut message, false);
        self.inspector = inspector;
        let gas_spent = SYSTEM_CALL_GAS_LIMIT.saturating_sub(result.gas.remaining());
        let gas_refunded = if result.stop.is_success() && result.gas.refunded() > 0 {
            result.gas.refunded() as u64
        } else {
            0
        };
        let gas_used = gas_spent.saturating_sub(gas_refunded);
        let mut outcome = TxResult {
            status: result.stop.is_success(),
            gas_used,
            stop: result.stop,
            output: result.output,
            ..TxResult::default()
        };

        let has_pending_state = if let Err(stop) = self.finalize_transaction() {
            outcome.status = false;
            outcome.stop = stop;
            outcome.output = Bytes::new();
            outcome.logs.clear();
            self.state.clear_transaction_state();
            false
        } else {
            outcome.logs = self.state.take_logs();
            true
        };
        outcome.db_error_code = self.db_error_code();
        ExecutedTx::from_result(self, outcome, has_pending_state)
    }

    /// Executes a system call from `caller` to `system_contract_address` on an async fiber.
    ///
    /// This must be used with an async database adapter such as
    /// [`evm::async::AsyncDb`](crate::evm::async::AsyncDb) to take
    /// advantage of yielding database I/O. With a synchronous database this is mostly equivalent to
    /// running the synchronous system call on a fiber.
    ///
    /// This returns a `Send` future. Before calling it, the current erased database, precompile
    /// provider, and optional inspector must be verified with [`Evm::evm_is_send`] or
    /// [`Evm::evm_is_send_with_inspector`].
    #[cfg(feature = "async")]
    #[inline]
    pub fn system_call_with_caller_async(
        &mut self,
        caller: Address,
        system_contract_address: Address,
        data: Bytes,
    ) -> impl Future<Output = r#async::AsyncResult<TxResult<T>, Infallible>> + Send + '_
    where
        T::TxResultExt: Send,
    {
        self.assert_erased_send();
        let stack = self.async_stack();
        let mut evm = SendEvmRef::new(self);
        // SAFETY: The returned future owns the exclusive `&mut self` borrow, so nothing else can
        // access the EVM stack slot until that future is dropped. The send marker checked above
        // requires all erased EVM fields to have been verified by `Evm::evm_is_send`.
        unsafe {
            r#async::on_fiber_with_stack(stack, move || {
                evm.system_call_with_caller(caller, system_contract_address, data)
            })
        }
    }
}

#[cfg(feature = "async")]
impl<T: EvmTypes<Host = Evm<T>>> SendEvmRef<'_, T> {
    #[inline]
    fn system_call(&mut self, system_contract_address: Address, data: Bytes) -> TxResult<T> {
        self.evm.system_call(system_contract_address, data).commit()
    }

    #[inline]
    fn system_call_with_caller(
        &mut self,
        caller: Address,
        system_contract_address: Address,
        data: Bytes,
    ) -> TxResult<T> {
        self.evm.system_call_with_caller(caller, system_contract_address, data).commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Precompiles, SpecId,
        bytecode::Bytecode,
        env::BlockEnv,
        evm::{AccountInfo, InMemoryDB},
        interpreter::{InstrStop, op},
        registry::TxRegistry,
    };

    type TestEvm = Evm<BaseEvmTypes>;

    #[test]
    fn system_call_uses_system_sender_without_fee_accounting() {
        let contract = Address::from([0x42; 20]);
        let beneficiary = Address::from([0x99; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::CALLER,
            op::PUSH1,
            0,
            op::SSTORE,
            op::ORIGIN,
            op::PUSH1,
            1,
            op::SSTORE,
            op::STOP,
        ]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let block = BlockEnv { beneficiary, basefee: U256::from(7), ..BlockEnv::default() };
        let mut evm = TestEvm::new(
            SpecId::OSAKA,
            block,
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::OSAKA),
        );

        let result = evm.system_call(contract, Bytes::new()).detach();

        assert!(result.result.status);
        assert!(result.result.gas_used < SYSTEM_CALL_GAS_LIMIT);
        let unchanged = |address| !result.state_changes.accounts.contains_key(address);
        assert!(unchanged(&SYSTEM_ADDRESS));
        assert!(unchanged(&beneficiary));
        let storage = &result.state_changes.storage.get(&contract).expect("storage changed").slots;
        let system_address = U256::from_be_slice(SYSTEM_ADDRESS.as_slice());
        assert_eq!(storage.get(&U256::ZERO).map(|slot| slot.current), Some(system_address));
        assert_eq!(storage.get(&U256::ONE).map(|slot| slot.current), Some(system_address));
    }

    #[test]
    fn system_call_starts_with_warm_system_contract() {
        let contract = Address::from([0x42; 20]);
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::ADDRESS, op::EXTCODESIZE, op::STOP]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let mut evm = TestEvm::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::OSAKA),
        );

        let outcome = evm.system_call(contract, Bytes::new()).discard();

        assert!(outcome.status);
        assert!(
            outcome.gas_used < 1_000,
            "system contract should be warm before execution, got {} gas used",
            outcome.gas_used
        );
    }

    #[test]
    fn system_call_to_missing_code_is_noop() {
        let contract = Address::from([0x42; 20]);
        let mut evm = TestEvm::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );

        let result = evm.system_call(contract, Bytes::new()).detach();

        assert!(result.result.status);
        assert_eq!(result.result.gas_used, 0);
        assert!(result.state_changes.is_empty());
    }

    #[test]
    fn system_call_reverts_state_changes() {
        let contract = Address::from([0x42; 20]);
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            1,
            op::PUSH1,
            0,
            op::SSTORE,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::REVERT,
        ]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let mut evm = TestEvm::new(
            SpecId::OSAKA,
            BlockEnv::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::OSAKA),
        );

        let result = evm.system_call(contract, Bytes::new()).detach();

        assert!(!result.result.status);
        assert_eq!(result.result.stop, InstrStop::Revert);
        assert!(result.state_changes.is_empty());
    }
}
