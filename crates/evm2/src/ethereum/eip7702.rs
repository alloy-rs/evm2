use super::{
    InitialFrame, LazyAuthorization, PreparedTx, access_list_counts, effective_gas_price,
    floor_gas, initial_gas_and_reservoir, intrinsic_gas, prepare_initial_frame, runtime_oog_result,
    settle_initial_frame_gas, validate_block_gas_limit, validate_chain_id,
    validate_create_initcode, validate_execution_gas_limit_cap, validate_floor_gas,
    validate_gas_price, validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
    validate_sender, validate_tx_gas_limit_cap, warm_access_list, warm_base_accounts,
};
use crate::{
    Evm, EvmFeatures, EvmTypes, TxResult, Version,
    env::TxEnvExt,
    evm::handler::{DefaultTxHandlerHooks, GasSettlement, TxHandlerHooks},
    interpreter::{GasTracker, Host, InstrStop, MessageResult, gas::EIP8038_ACCOUNT_WRITE},
    registry::{HandlerError, HandlerResult, TxRequest},
    version::GasId,
};
use alloc::vec::Vec;
use alloy_primitives::{Address, U256};

/// Executes an EIP-7702 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(
    req: TxRequest<'_, '_, T, super::LazyTxEip7702>,
) -> HandlerResult<TxResult<T>> {
    handle_with_hooks::<T, DefaultTxHandlerHooks>(req)
}

/// Executes an EIP-7702 transaction using Ethereum rules and custom handler hooks.
pub fn handle_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    mut req: TxRequest<'_, '_, T, super::LazyTxEip7702>,
) -> HandlerResult<TxResult<T>> {
    let prepared = prepare_with_hooks::<T, H>(&mut req)?;
    execute_prepared::<T, H>(req, prepared)
}

/// Validates an EIP-7702 transaction and applies its pre-execution state changes.
pub fn prepare_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: &mut TxRequest<'_, '_, T, super::LazyTxEip7702>,
) -> HandlerResult<PreparedTx> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let envelope = req.envelope;
    if tx.authorization_list.is_empty() {
        return Err(HandlerError::EmptyAuthorizationList);
    }
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to.into(), &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let mut intrinsic = intrinsic_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    ) + eip7702_authorization_gas(req.host, tx.authorization_list.len());
    // EIP-2780 (ethereum/EIPs#11844): the per-auth state-dependent charges are applied at the
    // runtime gas phase, so no state gas is charged at the intrinsic phase (pre-Amsterdam there is
    // none either). A transaction that passes the intrinsic check but cannot afford the runtime
    // charges is included as an out-of-gas halt rather than rejected.
    let mut initial_state_gas = 0;
    let mut floor_gas = floor_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    );
    H::adjust_intrinsic_gas(
        req.host,
        envelope,
        &mut intrinsic,
        &mut initial_state_gas,
        &mut floor_gas,
    )?;
    validate_intrinsic_gas(tx.gas_limit, intrinsic, initial_state_gas)?;
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_execution_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    validate_sender(req.host, caller, tx.nonce, max_gas_cost.saturating_add(tx.value))?;

    warm_base_accounts(req.host, caller, tx.to.into());
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    req.host.state.account(&caller)?.bump_nonce();
    H::before_execution(req.host, envelope, caller, effective_gas_cost)?;

    Ok(PreparedTx { caller, gas_price, intrinsic, initial_state_gas, floor_gas })
}

/// Executes and settles a prepared EIP-7702 transaction.
pub fn execute_prepared<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: TxRequest<'_, '_, T, super::LazyTxEip7702>,
    prepared: PreparedTx,
) -> HandlerResult<TxResult<T>> {
    let PreparedTx { caller, gas_price, intrinsic, initial_state_gas, floor_gas } = prepared;
    let tx = req.tx.inner();
    let envelope = req.envelope;
    let chain_id = req.host.version().chain_id;
    let tx_env = TxEnvExt {
        origin: caller,
        gas_price,
        chain_id: U256::from(chain_id),
        ..TxEnvExt::default()
    };

    let (execution_gas_limit, reservoir) =
        initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, initial_state_gas);
    let mut tx_gas =
        GasTracker::new_with_execution_gas_and_reservoir(execution_gas_limit, reservoir);
    // The delegations span `runtime_checkpoint` so a runtime out-of-gas can drop them; the
    // recipient is read only afterwards (at first-frame creation), so it too stays out of the
    // EIP-7928 block access list on an authorization out-of-gas. Pre-Amsterdam nothing rolls
    // the checkpoint back.
    let runtime_checkpoint = req.host.state.checkpoint();

    let AuthorizationResult { out_of_gas: auth_oog, state_refund, execution_refund } =
        H::apply_authorizations(req.host, envelope, tx, caller, &mut tx_gas)?;
    // The shared handler settles returned refunds exactly once, preserving state gas.
    tx_gas.set_reservoir(tx_gas.reservoir() + state_refund);

    // Applies the pre-Amsterdam authorization execution refund (zero under EIP-2780) and settles
    // the transaction with the hook-provided intrinsic state gas (charged upfront, before
    // `runtime_checkpoint`, so it persists on every exit). Every exit below funnels through here.
    let settle = |host: &mut Evm<'_, T>, mut result: MessageResult<T>| {
        result.gas.set_refunded(
            result
                .gas
                .refunded()
                .saturating_add(i64::try_from(execution_refund).unwrap_or(i64::MAX)),
        );
        H::settle_transaction(
            host,
            envelope,
            GasSettlement {
                caller,
                gas_price,
                gas_limit: tx.gas_limit,
                floor_gas,
                initial_state_gas,
                state_refund,
                result,
            },
        )
    };
    // Settles the transaction as an out-of-gas halt when the runtime gas phase (the authorization
    // charges or the first-frame recipient charge) runs out of gas: reverts the authorization
    // checkpoint to drop the applied delegations, then consumes all execution gas and returns the
    // reservoir. Unreachable pre-Amsterdam, where no runtime charge is attempted.
    let settle_oog = |host: &mut Evm<'_, T>| {
        let features = host.version().features;
        host.state.rollback(runtime_checkpoint, features);
        settle(host, runtime_oog_result(execution_gas_limit, reservoir))
    };

    if auth_oog {
        return settle_oog(req.host);
    }
    let Some(InitialFrame { mut message, charged_state_gas }) = prepare_initial_frame(
        req.host,
        caller,
        tx.nonce,
        tx.to.into(),
        &tx.input,
        tx.value,
        &mut tx_gas,
    )?
    else {
        // A depth-0 recipient charge that ran out of gas is part of the runtime gas phase, so it
        // drops the delegations too.
        return settle_oog(req.host);
    };

    // Failed execution has already been rolled back to the message's own checkpoint (past the
    // applied delegations, which stay) inside `execute_message`. The settle merges the frame gas
    // into `tx_gas`, which carries the authorization state gas into the block state-gas
    // accounting.
    let mut result = req.host.execute_message(&tx_env, &mut message)?;
    settle_initial_frame_gas(&mut tx_gas, &mut result, charged_state_gas);
    settle(req.host, result)
}

fn eip7702_authorization_gas<'a, T: EvmTypes>(host: &Evm<'a, T>, authorizations: usize) -> u64 {
    let per_auth = u64::from(host.version().gas_params.get(GasId::TxEip7702PerEmptyAccountCost));
    (authorizations as u64).saturating_mul(per_auth)
}

/// Outcome of validating one EIP-7702 authorization, carrying the facts needed to compute its gas
/// charges (execution-specs `set_delegation`).
#[derive(Clone, Copy, Debug)]
pub struct AppliedAuth {
    /// Whether the authority account already existed when this authorization was processed.
    pub existed: bool,
    /// Whether the authority's code was a valid delegation at the start of the transaction.
    pub delegated_before_tx: bool,
    /// Whether the authority's code was a valid delegation when this authorization was processed
    /// (i.e. as left by an earlier authorization for the same authority in this transaction).
    pub delegated_now: bool,
    /// Whether this authorization clears the delegation (target is the zero address).
    pub clearing: bool,
}

/// Validates one authorization against current state without applying it. Returns
/// `Some((authority, facts))` for an accepted authorization or `None` for a rejected one. Mirrors
/// execution-specs `validate_authorization`.
pub fn validate_one_auth<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorization: &LazyAuthorization,
) -> HandlerResult<Option<(Address, AppliedAuth)>> {
    if !authorization.chain_id().is_zero() && authorization.chain_id() != &U256::from(chain_id) {
        return Ok(None);
    }
    if authorization.nonce() == u64::MAX {
        return Ok(None);
    }
    let Some(authority) = authorization.authority() else {
        return Ok(None);
    };
    let chunked = host.feature(EvmFeatures::TIP1143);
    let mut account = host.state.account(&authority)?;
    account.warm();
    let existed = account.exists();
    let authority_nonce = account.nonce();
    let delegated_now = if chunked {
        let delegated = account.code_is_eip7702()?;
        if account.code_size() != Some(0) && !delegated {
            return Ok(None);
        }
        delegated
    } else {
        let code = account.load_code()?;
        if !code.is_empty() && !code.is_eip7702() {
            return Ok(None);
        }
        !code.is_empty()
    };
    if authorization.nonce() != authority_nonce {
        return Ok(None);
    }
    let delegated_before_tx = if chunked {
        account.original_code_is_eip7702()?
    } else {
        account.original_code()?.is_eip7702()
    };
    let clearing = authorization.address().is_zero();
    Ok(Some((authority, AppliedAuth { existed, delegated_before_tx, delegated_now, clearing })))
}

/// Gas accounting for one regime of [`apply_auth_list`].
///
/// The loop validates each authorization and applies the accepted ones; the accounting decides
/// what each outcome costs or refunds. [`RuntimeAuthCharges`] meters the EIP-2780 runtime charges
/// and can abort the list, [`AuthRefunds`] accumulates the pessimistic-intrinsic refunds and never
/// fails; handlers with their own authorization pricing supply their own implementation.
pub trait AuthAccounting {
    /// Called for a rejected authorization before moving to the next entry.
    fn rejected(&mut self);

    /// Called for an accepted authorization before its delegation is applied. Returning
    /// out-of-gas aborts the list: the delegation is not applied and no later authority is
    /// loaded.
    fn accepted(&mut self, authority: Address, auth: &AppliedAuth) -> Result<(), InstrStop>;
}

/// EIP-2780 runtime accounting: meters the state-dependent charges on the transaction-level gas
/// tracker as the delegations are applied (ethereum/EIPs#11844, #11891).
///
/// Per accepted authority: the new-account state gas when the authority does not exist,
/// `ACCOUNT_WRITE` execution gas on the first write to the authority's leaf (unless already paid —
/// the sender at inclusion, the recipient of a value-bearing transaction, or a preceding valid
/// authorization on the same authority), and the net-new delegation-indicator state gas.
///
/// Rejected authorizations charge nothing (the intrinsic `REGULAR_PER_AUTH_BASE_COST` already
/// covers their work) and are not refunded.
#[derive(Debug)]
pub struct RuntimeAuthCharges<'g> {
    gas: &'g mut GasTracker,
    new_account_state_gas: u64,
    delegation_bytes_state_gas: u64,
    account_write_cost: u64,
    /// Accounts whose leaf write this transaction has already paid for.
    written: Vec<Address>,
    /// Authorities whose net-new delegation bytes were already charged; the charge applies at most
    /// once per authority (covering a set-clear-set sequence within one transaction).
    charged_delegation_bytes: Vec<Address>,
}

impl<'g> RuntimeAuthCharges<'g> {
    /// Creates the runtime accounting for a transaction from `caller` to `recipient` carrying
    /// `value`. The sender's leaf write is priced into `TX_BASE` and the value-bearing recipient's
    /// into `TX_VALUE_COST`, so neither pays `ACCOUNT_WRITE` again.
    pub fn new(
        version: &Version,
        gas: &'g mut GasTracker,
        caller: Address,
        recipient: Address,
        value: U256,
    ) -> Self {
        let mut written = Vec::new();
        written.push(caller);
        if !value.is_zero() {
            written.push(recipient);
        }
        // State gas only exists under EIP-8037.
        let is_eip8037 = version.feature(EvmFeatures::EIP8037);
        Self {
            gas,
            new_account_state_gas: if is_eip8037 {
                version.gas_params.new_account_state_gas()
            } else {
                0
            },
            delegation_bytes_state_gas: if is_eip8037 {
                u64::from(version.gas_params.get(GasId::TxEip7702PerAuthState))
            } else {
                0
            },
            account_write_cost: u64::from(EIP8038_ACCOUNT_WRITE),
            written,
            charged_delegation_bytes: Vec::new(),
        }
    }
}

impl AuthAccounting for RuntimeAuthCharges<'_> {
    fn rejected(&mut self) {}

    fn accepted(&mut self, authority: Address, auth: &AppliedAuth) -> Result<(), InstrStop> {
        // Non-existent authority: pay for the new account leaf's state bytes.
        if !auth.existed {
            self.gas.spend_state(self.new_account_state_gas)?;
        }
        // First write to the authority's leaf within the transaction pays `ACCOUNT_WRITE`.
        if !self.written.contains(&authority) {
            self.gas.spend(self.account_write_cost)?;
            self.written.push(authority);
        }
        // Net-new delegation bytes: the 23-byte designator written into a previously empty slot.
        if !auth.clearing
            && !auth.delegated_now
            && !auth.delegated_before_tx
            && !self.charged_delegation_bytes.contains(&authority)
        {
            self.gas.spend_state(self.delegation_bytes_state_gas)?;
            self.charged_delegation_bytes.push(authority);
        }
        Ok(())
    }
}

/// Pre-EIP-2780 accounting: refunds against the pessimistic per-authorization intrinsic charge.
///
/// Follows execution-specs `set_delegation`. The per-authorization state and execution gas charged
/// in the intrinsic cost is refilled when it turns out not to be needed: the state refund is
/// credited to the reservoir (so it stays state gas) and the execution refund is routed through
/// the capped refund counter.
///
/// Before EIP-8037 (Prague) there is no state gas: only the per-existing-account execution refund
/// applies and rejected authorizations refund nothing.
#[derive(Clone, Copy, Debug)]
pub struct AuthRefunds {
    is_eip8037: bool,
    new_account: u64,
    auth_base: u64,
    execution_per_auth: u64,
    /// Accumulated state-gas refund.
    pub state_refund: u64,
    /// Accumulated execution-gas refund.
    pub execution_refund: u64,
}

impl AuthRefunds {
    /// Creates refund accounting with `version`'s authorization prices.
    pub fn new(version: &Version) -> Self {
        Self {
            is_eip8037: version.feature(EvmFeatures::EIP8037),
            new_account: version.gas_params.new_account_state_gas(),
            auth_base: u64::from(version.gas_params.get(GasId::TxEip7702PerAuthState)),
            execution_per_auth: u64::from(version.gas_params.get(GasId::TxEip7702AuthRefund)),
            state_refund: 0,
            execution_refund: 0,
        }
    }
}

impl AuthAccounting for AuthRefunds {
    fn rejected(&mut self) {
        // Rejected authorization. Under EIP-8037 its full intrinsic state gas (account + bytecode)
        // refills the reservoir and the speculative account write is refunded; before EIP-8037
        // nothing is refunded.
        if self.is_eip8037 {
            self.state_refund = self.state_refund.saturating_add(self.new_account + self.auth_base);
            self.execution_refund = self.execution_refund.saturating_add(self.execution_per_auth);
        }
    }

    fn accepted(&mut self, _authority: Address, auth: &AppliedAuth) -> Result<(), InstrStop> {
        // Existing authority: the worst-case `ACCOUNT_WRITE` execution gas was not needed. This
        // refund applies in every regime (it is the only authorization refund before EIP-8037).
        if auth.existed {
            self.execution_refund = self.execution_refund.saturating_add(self.execution_per_auth);
        }

        // The remaining refunds are state gas, which only exists under EIP-8037.
        if !self.is_eip8037 {
            return Ok(());
        }

        let mut refund = 0u64;
        // Existing authority: its `NEW_ACCOUNT` state gas was not needed.
        if auth.existed {
            refund += self.new_account;
        }
        // Bytecode (`AUTH_BASE`) refunds.
        if auth.clearing {
            refund += self.auth_base;
            // Clearing a delegation freshly installed earlier in this transaction refills the
            // bytecode state gas a second time.
            if auth.delegated_now && !auth.delegated_before_tx {
                refund += self.auth_base;
            }
        } else if auth.delegated_now || auth.delegated_before_tx {
            refund += self.auth_base;
        }
        self.state_refund = self.state_refund.saturating_add(refund);
        Ok(())
    }
}

/// Validates and applies an EIP-7702 authorization list, driving `accounting` with each
/// per-authorization outcome.
///
/// Each authorization is validated against current state ([`validate_one_auth`]); the accounting
/// is told about rejected entries and charges (or accumulates refunds) for accepted ones before
/// their delegation is applied. An accounting out-of-gas aborts the list without loading the
/// remaining authorities — keeping them out of the EIP-7928 block access list — and returns
/// `true`; the caller is responsible for rolling back the partially applied delegations.
pub fn apply_auth_list<'a, T: EvmTypes>(
    host: &mut Evm<'a, T>,
    chain_id: u64,
    authorizations: &[LazyAuthorization],
    accounting: &mut impl AuthAccounting,
) -> HandlerResult<bool> {
    for authorization in authorizations {
        let Some((authority, auth)) = validate_one_auth(host, chain_id, authorization)? else {
            accounting.rejected();
            continue;
        };
        if accounting.accepted(authority, &auth).is_err() {
            return Ok(true);
        }
        host.state.account(&authority)?.set_delegation(*authorization.address());
    }
    Ok(false)
}

/// Gas accounting produced while applying an authorization list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorizationResult {
    /// Whether a runtime charge failed. The handler rolls back the authorization checkpoint.
    pub out_of_gas: bool,
    /// Intrinsic state gas to return directly to the transaction's reservoir.
    pub state_refund: u64,
    /// Execution gas refund to include during transaction settlement.
    pub execution_refund: u64,
}

/// Applies authorizations with Ethereum's fork-dependent gas accounting.
///
/// EIP-2780 meters runtime charges as delegations are applied, stopping before loading later
/// authorities if a charge fails. Earlier forks refund pessimistic intrinsic costs instead.
/// The caller must credit the returned refunds and roll back delegations on out-of-gas.
pub fn apply_authorizations<T: EvmTypes>(
    host: &mut Evm<'_, T>,
    tx: &super::LazyTxEip7702,
    caller: Address,
    gas: &mut GasTracker,
) -> HandlerResult<AuthorizationResult> {
    let chain_id = host.version().chain_id;
    if host.feature(EvmFeatures::EIP2780) {
        let mut charges = RuntimeAuthCharges::new(host.version(), gas, caller, tx.to, tx.value);
        let out_of_gas = apply_auth_list(host, chain_id, &tx.authorization_list, &mut charges)?;
        Ok(AuthorizationResult { out_of_gas, ..Default::default() })
    } else {
        let mut refunds = AuthRefunds::new(host.version());
        apply_auth_list(host, chain_id, &tx.authorization_list, &mut refunds)?;
        Ok(AuthorizationResult {
            state_refund: refunds.state_refund,
            execution_refund: refunds.execution_refund,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, ExecutionConfig, Precompiles, SpecId,
        env::BlockEnvExt,
        ethereum::{LazyTxEip7702, TxEnvelope},
        evm::{AccountInfo, InMemoryDB},
        registry::{TxRegistry, handler},
    };
    use alloc::vec;
    use alloy_consensus::{TxEip7702, transaction::Recovered};
    use alloy_eips::eip7702::{Authorization, RecoveredAuthority, RecoveredAuthorization};
    use core::cell::Cell;

    std::thread_local! {
        static HOOK_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    struct DelegateHook;
    struct RejectHook;

    impl TxHandlerHooks<BaseEvmTypes> for DelegateHook {
        fn apply_authorizations(
            host: &mut Evm<'_, BaseEvmTypes>,
            envelope: &TxEnvelope,
            tx: &LazyTxEip7702,
            caller: Address,
            gas: &mut GasTracker,
        ) -> HandlerResult<AuthorizationResult> {
            HOOK_CALLS.with(|calls| calls.set(calls.get() + 1));
            assert!(core::ptr::eq(envelope.as_eip7702().unwrap(), tx));
            apply_authorizations(host, tx, caller, gas)
        }
    }

    impl TxHandlerHooks<BaseEvmTypes> for RejectHook {
        fn apply_authorizations(
            host: &mut Evm<'_, BaseEvmTypes>,
            envelope: &TxEnvelope,
            tx: &LazyTxEip7702,
            caller: Address,
            gas: &mut GasTracker,
        ) -> HandlerResult<AuthorizationResult> {
            let result = DelegateHook::apply_authorizations(host, envelope, tx, caller, gas)?;
            assert!(!result.out_of_gas);
            assert_eq!(host.state.account(&Address::repeat_byte(0xcc))?.nonce(), 1);
            Ok(AuthorizationResult { out_of_gas: true, ..Default::default() })
        }
    }

    fn execute_hook<H: TxHandlerHooks<BaseEvmTypes> + 'static>(reject: bool) {
        HOOK_CALLS.with(|calls| calls.set(0));
        let caller = Address::repeat_byte(0xbb);
        let authority = Address::repeat_byte(0xcc);
        let delegated = Address::repeat_byte(0xdd);
        let spec = SpecId::PRAGUE;
        let mut db = InMemoryDB::default();
        db.insert_account_info(&caller, AccountInfo::default().with_balance(U256::from(u64::MAX)));
        let mut registry = TxRegistry::new();
        registry.register(
            4,
            TxEnvelope::as_eip7702,
            handler(prepare_with_hooks::<BaseEvmTypes, H>, execute_prepared::<BaseEvmTypes, H>),
        );
        let mut evm = Evm::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(spec, Version::new(spec)),
            spec,
            BlockEnvExt::default(),
            registry,
            db,
            Precompiles::base(spec),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Eip7702(LazyTxEip7702::from_cached_recovered_authorizations(
                TxEip7702 {
                    chain_id: 1,
                    to: Address::repeat_byte(0xaa),
                    gas_limit: 100_000,
                    ..Default::default()
                },
                vec![RecoveredAuthorization::new_unchecked(
                    Authorization { chain_id: U256::ZERO, address: delegated, nonce: 0 },
                    RecoveredAuthority::Valid(authority),
                )],
            )),
            caller,
        );
        let result = evm.transact(&tx).unwrap().detach();
        HOOK_CALLS.with(|calls| assert_eq!(calls.get(), 1));
        assert_eq!(result.result.status, !reject);
        assert_eq!(result.result.total_gas_spent, if reject { 100_000 } else { 46_000 });
        assert_eq!(result.result.state_gas_spent, 0);
        if reject {
            assert!(result.pending_state.account_info(&authority).is_none());
        } else {
            assert_eq!(result.pending_state.account_info(&authority).unwrap().nonce, 1);
        }
    }

    #[test]
    fn authorization_hook_dispatch_and_default_accounting() {
        execute_hook::<DelegateHook>(false);
    }

    #[test]
    fn authorization_hook_oog_reverts_applied_delegation() {
        execute_hook::<RejectHook>(true);
    }
}
