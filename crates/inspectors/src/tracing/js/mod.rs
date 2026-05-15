use crate::tracing::{
    TransactionContext,
    config::TraceStyle,
    js::{
        bindings::{
            CallFrame, Contract, EvmDbRef, FrameResult, JsEvmContext, MemoryRef, OpObj, StackRef,
            StepLog,
        },
        builtins::{PrecompileList, to_serde_value},
    },
    types::CallKind,
    utils,
};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use alloy_primitives::{Address, Bytes, TxKind, U256, map::HashSet};
pub use boa_engine::vm::RuntimeLimits;
use boa_engine::{Context, JsError, JsObject, JsValue, Source, js_string};
use evm2::{
    Evm, EvmTypes, Inspector, TxResult,
    env::BlockEnv,
    ethereum::RecoveredTxEnvelope,
    evm::DynDatabase,
    interpreter::{
        GasTracker, InstrStop, Interpreter, Message, MessageKind, MessageResult, opcode::op,
    },
};

pub(crate) mod bindings;
pub(crate) mod builtins;

use builtins::register_builtins;

/// The maximum number of iterations in a loop.
///
/// Once exceeded, the loop will throw an error.
pub const LOOP_ITERATION_LIMIT: u64 = 200_000;

/// The recursion limit for function calls.
///
/// Once exceeded, the loop will throw an error.
pub const RECURSION_LIMIT: usize = 10_000;

/// A javascript inspector that will delegate inspector functions to javascript functions
///
/// See also <https://geth.ethereum.org/docs/developers/evm-tracing/custom-tracer#custom-javascript-tracing>
#[derive(Debug)]
pub struct JsInspector {
    ctx: Context,
    code: String,
    _js_config_value: JsValue,
    config: serde_json::Value,
    obj: JsObject,
    result_fn: JsObject,
    fault_fn: JsObject,
    enter_fn: Option<JsObject>,
    exit_fn: Option<JsObject>,
    step_fn: Option<JsObject>,
    previous_gas_spent: u64,
    call_stack: Vec<CallStackItem>,
    transaction_context: TransactionContext,
    precompiles_registered: bool,
    last_start_step_pc: Option<usize>,
}

impl JsInspector {
    /// Creates a new inspector from a javascript code snipped that evaluates to an object with
    /// the expected fields and a config object.
    ///
    /// The object must have the following fields:
    ///  - `result`: a function that will be called when the result is requested.
    ///  - `fault`: a function that will be called when the transaction fails.
    ///
    /// Optional functions are invoked during inspection:
    /// - `setup`: a function that will be called before the inspection starts.
    /// - `enter`: a function that will be called when the execution enters a new call.
    /// - `exit`: a function that will be called when the execution exits a call.
    /// - `step`: a function that will be called when the execution steps to the next instruction.
    pub fn new(code: String, config: serde_json::Value) -> Result<Self, JsInspectorError> {
        Self::with_transaction_context(code, config, Default::default())
    }

    /// Creates a new inspector from a javascript code snippet. See also [Self::new].
    pub fn with_transaction_context(
        code: String,
        config: serde_json::Value,
        transaction_context: TransactionContext,
    ) -> Result<Self, JsInspectorError> {
        let mut ctx = Context::default();

        ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
        ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);

        register_builtins(&mut ctx)?;

        let wrapped = alloc::format!("({code})");
        let obj =
            ctx.eval(Source::from_bytes(wrapped.as_bytes())).map_err(JsInspectorError::EvalCode)?;

        let obj = obj.as_object().ok_or(JsInspectorError::ExpectedJsObject)?;

        let result_fn = obj
            .get(js_string!("result"), &mut ctx)?
            .as_object()
            .ok_or(JsInspectorError::ResultFunctionMissing)?;
        if !result_fn.is_callable() {
            return Err(JsInspectorError::ResultFunctionMissing);
        }

        let fault_fn = obj
            .get(js_string!("fault"), &mut ctx)?
            .as_object()
            .ok_or(JsInspectorError::FaultFunctionMissing)?;
        if !fault_fn.is_callable() {
            return Err(JsInspectorError::FaultFunctionMissing);
        }

        let enter_fn =
            obj.get(js_string!("enter"), &mut ctx)?.as_object().filter(|o| o.is_callable());
        let exit_fn =
            obj.get(js_string!("exit"), &mut ctx)?.as_object().filter(|o| o.is_callable());
        let step_fn =
            obj.get(js_string!("step"), &mut ctx)?.as_object().filter(|o| o.is_callable());

        let _js_config_value =
            JsValue::from_json(&config, &mut ctx).map_err(JsInspectorError::InvalidJsonConfig)?;

        if let Some(setup_fn) = obj.get(js_string!("setup"), &mut ctx)?.as_object() {
            if !setup_fn.is_callable() {
                return Err(JsInspectorError::SetupFunctionNotCallable);
            }

            setup_fn
                .call(&(obj.clone().into()), core::slice::from_ref(&_js_config_value), &mut ctx)
                .map_err(JsInspectorError::SetupCallFailed)?;
        }

        Ok(Self {
            ctx,
            code,
            _js_config_value,
            config,
            obj,
            result_fn,
            fault_fn,
            enter_fn,
            exit_fn,
            step_fn,
            previous_gas_spent: 0,
            call_stack: Vec::new(),
            transaction_context,
            precompiles_registered: false,
            last_start_step_pc: None,
        })
    }

    /// Returns the transaction context.
    pub const fn transaction_context(&self) -> &TransactionContext {
        &self.transaction_context
    }

    /// Set contextual transaction info.
    pub const fn set_transaction_context(&mut self, transaction_context: TransactionContext) {
        self.transaction_context = transaction_context;
    }

    /// Applies runtime limits to the JS context.
    pub fn set_runtime_limits(&mut self, limits: RuntimeLimits) {
        self.ctx.set_runtime_limits(limits);
    }

    /// Returns the javascript source code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the javascript tracer config.
    pub const fn config(&self) -> &serde_json::Value {
        &self.config
    }

    /// Creates a fresh copy of this inspector, resetting all execution state.
    pub fn try_clone(&self) -> Result<Self, JsInspectorError> {
        Self::new(self.code.clone(), self.config.clone())
    }

    /// Calls the result function and returns the result as [`serde_json::Value`].
    pub fn json_result<T: EvmTypes>(
        &mut self,
        result: &TxResult<T>,
        tx: &RecoveredTxEnvelope,
        block: &BlockEnv,
        db: &mut dyn DynDatabase,
    ) -> Result<serde_json::Value, JsInspectorError> {
        let result = self.result(result, tx, block, db)?;
        Ok(to_serde_value(result, &mut self.ctx)?)
    }

    /// Calls the result function and returns the result.
    pub fn result<T: EvmTypes>(
        &mut self,
        result: &TxResult<T>,
        tx: &RecoveredTxEnvelope,
        block: &BlockEnv,
        db: &mut dyn DynDatabase,
    ) -> Result<JsValue, JsInspectorError> {
        let mut to = None;
        let mut error = None;

        if result.status {
            to = result.created_address;
        } else if result.stop.is_revert() {
            error = Some("execution reverted".to_string());
        } else {
            error = Some(format!("execution halted: {:?}", result.stop));
        }

        let kind = tx.kind();
        if let TxKind::Call(target) = kind {
            to = Some(target);
        }

        let base_fee = block.basefee.try_into().unwrap_or(u64::MAX);

        let ctx = JsEvmContext {
            r#type: match kind {
                TxKind::Call(_) => "CALL",
                TxKind::Create => "CREATE",
            }
            .to_string(),
            from: tx.signer(),
            to,
            input: tx.input().clone(),
            gas: tx.gas_limit(),
            gas_used: result.gas_used,
            gas_price: tx.effective_gas_price(Some(base_fee)).try_into().unwrap_or(u64::MAX),
            value: tx.value(),
            block: block.number.try_into().unwrap_or(u64::MAX),
            coinbase: block.beneficiary,
            output: result.output.clone(),
            time: block.timestamp.to_string(),
            intrinsic_gas: 0,
            transaction_ctx: self.transaction_context,
            error,
        };
        let ctx = ctx.into_js_object(&mut self.ctx)?;
        let (db, _db_guard) = EvmDbRef::new_changes(&result.state_changes, db);
        let db = db.into_js_object(&mut self.ctx)?;
        Ok(self.result_fn.call(
            &(self.obj.clone().into()),
            &[ctx.into(), db.into()],
            &mut self.ctx,
        )?)
    }

    const fn get_op_cost(&self, spent: u64) -> u64 {
        spent.saturating_sub(self.previous_gas_spent)
    }

    const fn set_previous_gas_spent(&mut self, spent: u64) {
        self.previous_gas_spent = spent;
    }

    #[track_caller]
    fn active_call(&self) -> &CallStackItem {
        self.call_stack.last().expect("call stack is empty")
    }

    fn push_call<T: EvmTypes>(&mut self, message: &Message<T>) {
        let (caller, contract) = match message.kind {
            MessageKind::CallCode | MessageKind::DelegateCall => {
                (message.destination, message.code_address)
            }
            _ => (message.caller, message.destination),
        };
        let call = CallStackItem {
            contract: Contract {
                caller,
                contract,
                value: message.value,
                input: message.input.clone(),
            },
            kind: message.kind.into(),
            gas_limit: message.gas_limit,
        };
        self.call_stack.push(call);
    }

    fn pop_call(&mut self) {
        let _ = self.call_stack.pop();
    }

    const fn is_root_call_active(&self) -> bool {
        self.call_stack.len() == 1
    }

    const fn can_call_enter(&self) -> bool {
        self.enter_fn.is_some() && !self.is_root_call_active()
    }

    const fn can_call_exit(&self) -> bool {
        self.exit_fn.is_some() && !self.is_root_call_active()
    }

    fn try_step(&mut self, step: StepLog, db: EvmDbRef) -> Result<(), JsError> {
        let Some(step_fn) = &self.step_fn else { return Ok(()) };
        let js_step = step.into_js_object(&mut self.ctx)?;
        let db = db.into_js_object(&mut self.ctx)?;
        step_fn.call(&(self.obj.clone().into()), &[js_step.into(), db.into()], &mut self.ctx)?;
        Ok(())
    }

    fn try_fault(&mut self, step: StepLog, db: EvmDbRef) -> Result<(), JsError> {
        let js_step = step.into_js_object(&mut self.ctx)?;
        let db = db.into_js_object(&mut self.ctx)?;
        self.fault_fn.call(
            &(self.obj.clone().into()),
            &[js_step.into(), db.into()],
            &mut self.ctx,
        )?;
        Ok(())
    }

    fn try_enter(&mut self, frame: CallFrame) -> Result<(), JsError> {
        let Some(enter_fn) = &self.enter_fn else { return Ok(()) };
        let frame = frame.into_js_object(&mut self.ctx)?;
        enter_fn.call(&(self.obj.clone().into()), &[frame.into()], &mut self.ctx)?;
        Ok(())
    }

    fn try_exit(&mut self, frame_result: FrameResult) -> Result<(), JsError> {
        let Some(exit_fn) = &self.exit_fn else { return Ok(()) };
        let frame_result = frame_result.into_js_object(&mut self.ctx)?;
        exit_fn.call(&(self.obj.clone().into()), &[frame_result.into()], &mut self.ctx)?;
        Ok(())
    }

    fn register_precompiles<T: EvmTypes<Host = Evm<T>>>(&mut self, host: &Evm<T>) {
        if self.precompiles_registered {
            return;
        }
        let precompiles = PrecompileList(HashSet::from_iter(host.precompiles().warm_addresses()));
        let _ = precompiles.register_callable(&mut self.ctx);
        self.precompiles_registered = true;
    }
}

#[derive(Clone, Debug, Default)]
struct CallStackItem {
    contract: Contract,
    kind: CallKind,
    gas_limit: u64,
}

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for JsInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
        self.register_precompiles(host);
        self.last_start_step_pc = Some(interp.pc());

        if self.step_fn.is_none() {
            return;
        }

        let (db, db_guard) = EvmDbRef::new_state(host.state_mut());
        let (stack, stack_guard) = StackRef::new(interp.stack());
        let (memory, memory_guard) = MemoryRef::new(interp.memory_ref());
        let active_call = self.active_call();
        let message = interp.message();
        let gas_spent = interp.gas().spent();
        let step = StepLog {
            stack,
            op: OpObj(interp.opcode()),
            memory,
            pc: interp.pc() as u64,
            gas_remaining: interp.gas().remaining(),
            cost: self.get_op_cost(gas_spent),
            depth: u64::from(message.depth),
            refund: interp.gas().refunded().max(0) as u64,
            error: None,
            contract: Contract {
                caller: message.caller,
                contract: message.destination,
                value: active_call.contract.value,
                input: active_call.contract.input.clone(),
            },
        };

        self.set_previous_gas_spent(gas_spent);
        let step_result = self.try_step(step, db);
        drop(memory_guard);
        drop(stack_guard);
        drop(db_guard);

        if step_result.is_err() {
            interp.set_stop(InstrStop::Revert);
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
        if self.step_fn.is_none() || !matches!(interp.result(), Err(stop) if stop.is_revert()) {
            return;
        }

        let (db, _db_guard) = EvmDbRef::new_state(host.state_mut());
        let (stack, _stack_guard) = StackRef::new(interp.stack());
        let (memory, _memory_guard) = MemoryRef::new(interp.memory_ref());
        let active_call = self.active_call();
        let message = interp.message();
        let gas_spent = interp.gas().spent();
        let step = StepLog {
            stack,
            op: OpObj(op::REVERT),
            memory,
            pc: self.last_start_step_pc.unwrap_or_default() as u64,
            gas_remaining: interp.gas().remaining(),
            cost: self.get_op_cost(gas_spent),
            depth: u64::from(message.depth),
            refund: interp.gas().refunded().max(0) as u64,
            error: interp.result().err().map(|err| format!("{err:?}")),
            contract: Contract {
                caller: message.caller,
                contract: message.destination,
                value: active_call.contract.value,
                input: active_call.contract.input.clone(),
            },
        };

        let _ = self.try_fault(step, db);
    }

    fn call(&mut self, message: &mut Message<T>, host: &mut T::Host) -> Option<MessageResult<T>> {
        self.register_precompiles(host);
        self.push_call(message);
        if self.can_call_enter() {
            let call = self.active_call();
            let frame =
                CallFrame { contract: call.contract.clone(), kind: call.kind, gas: call.gas_limit };
            if let Err(err) = self.try_enter(frame) {
                return Some(js_error_to_revert(err));
            }
        }
        None
    }

    fn call_end(
        &mut self,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
        _host: &mut T::Host,
    ) {
        if self.can_call_exit() {
            let frame_result = FrameResult {
                gas_used: result.gas.spent(),
                output: result.output.clone(),
                error: utils::fmt_error_msg(result.stop, TraceStyle::Geth),
            };
            if let Err(err) = self.try_exit(frame_result) {
                *result = js_error_to_revert(err);
            }
        }

        self.pop_call();
    }

    fn create(&mut self, message: &mut Message<T>, host: &mut T::Host) -> Option<MessageResult<T>> {
        self.register_precompiles(host);
        self.push_call(message);
        if self.can_call_enter() {
            let call = self.active_call();
            let frame =
                CallFrame { contract: call.contract.clone(), kind: call.kind, gas: call.gas_limit };
            if let Err(err) = self.try_enter(frame) {
                return Some(js_error_to_revert(err));
            }
        }
        None
    }

    fn create_end(
        &mut self,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
        _host: &mut T::Host,
    ) {
        if self.can_call_exit() {
            let frame_result = FrameResult {
                gas_used: result.gas.spent(),
                output: result.output.clone(),
                error: None,
            };
            if let Err(err) = self.try_exit(frame_result) {
                *result = js_error_to_revert(err);
            }
        }

        self.pop_call();
    }

    fn selfdestruct(
        &mut self,
        _contract: &Address,
        _target: &Address,
        _value: &U256,
        _host: &mut T::Host,
    ) {
        if self.enter_fn.is_some() {
            let call = self.active_call();
            let frame =
                CallFrame { contract: call.contract.clone(), kind: call.kind, gas: call.gas_limit };
            let _ = self.try_enter(frame);
        }

        if self.exit_fn.is_some() {
            let frame_result = FrameResult { gas_used: 0, output: Bytes::new(), error: None };
            let _ = self.try_exit(frame_result);
        }
    }
}

fn js_error_to_revert<T: EvmTypes>(err: JsError) -> MessageResult<T> {
    MessageResult {
        stop: InstrStop::Revert,
        output: err.to_string().into_bytes().into(),
        gas: GasTracker::new(0),
        ..Default::default()
    }
}

/// Error variants that can occur during JavaScript inspection.
#[derive(Debug, thiserror::Error)]
pub enum JsInspectorError {
    /// Error originating from a JavaScript operation.
    #[error(transparent)]
    JsError(#[from] JsError),

    /// Failure during the evaluation of JavaScript code.
    #[error("failed to evaluate JS code: {0}")]
    EvalCode(JsError),

    /// The evaluated code is not a JavaScript object.
    #[error("the evaluated code is not a JS object")]
    ExpectedJsObject,

    /// The trace object must expose a function named `result()`.
    #[error("trace object must expose a function result()")]
    ResultFunctionMissing,

    /// The trace object must expose a function named `fault()`.
    #[error("trace object must expose a function fault()")]
    FaultFunctionMissing,

    /// The setup object must be a callable function.
    #[error("setup object must be a function")]
    SetupFunctionNotCallable,

    /// Failure during the invocation of the `setup()` function.
    #[error("failed to call setup(): {0}")]
    SetupCallFailed(JsError),

    /// Invalid JSON configuration encountered.
    #[error("invalid JSON config: {0}")]
    InvalidJsonConfig(JsError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::js::{bindings::JsEvmContext, builtins::to_serde_value};
    use alloc::{format, rc::Rc, string::ToString, vec, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, TxKind, U256, hex};
    use core::cell::RefCell;
    use evm2::{
        BaseEvmTypes, Evm, Precompiles, SpecId,
        bytecode::Bytecode,
        ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, CacheDB, EmptyDB},
    };
    use serde_json::json;

    #[derive(Clone)]
    struct SharedJsInspector(Rc<RefCell<JsInspector>>);

    impl Inspector<BaseEvmTypes> for SharedJsInspector {
        fn step(
            &mut self,
            interp: &mut Interpreter<'_, BaseEvmTypes>,
            host: &mut Evm<BaseEvmTypes>,
        ) {
            self.0.borrow_mut().step(interp, host);
        }

        fn step_end(
            &mut self,
            interp: &mut Interpreter<'_, BaseEvmTypes>,
            host: &mut Evm<BaseEvmTypes>,
        ) {
            self.0.borrow_mut().step_end(interp, host);
        }

        fn call(
            &mut self,
            message: &mut Message<BaseEvmTypes>,
            host: &mut Evm<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.0.borrow_mut().call(message, host)
        }

        fn call_end(
            &mut self,
            message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
            host: &mut Evm<BaseEvmTypes>,
        ) {
            self.0.borrow_mut().call_end(message, result, host);
        }

        fn create(
            &mut self,
            message: &mut Message<BaseEvmTypes>,
            host: &mut Evm<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.0.borrow_mut().create(message, host)
        }

        fn create_end(
            &mut self,
            message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
            host: &mut Evm<BaseEvmTypes>,
        ) {
            self.0.borrow_mut().create_end(message, result, host);
        }
    }

    fn js_result(
        insp: &mut JsInspector,
        result: &evm2::TxResult,
        target: Address,
        gas_price: u128,
        db: &mut dyn DynDatabase,
    ) -> serde_json::Value {
        let mut error = None;
        if !result.status {
            error = if result.stop.is_revert() {
                Some("execution reverted".to_string())
            } else {
                Some(format!("execution halted: {:?}", result.stop))
            };
        }
        let ctx = JsEvmContext {
            r#type: "CALL".to_string(),
            from: Address::ZERO,
            to: Some(target),
            input: Bytes::new(),
            gas: 1_000_000,
            gas_used: result.gas_used,
            gas_price: gas_price.try_into().unwrap_or(u64::MAX),
            value: U256::ZERO,
            block: 0,
            coinbase: Address::ZERO,
            output: result.output.clone(),
            time: "0".to_string(),
            intrinsic_gas: 0,
            transaction_ctx: TransactionContext::default(),
            error,
        };
        let ctx = ctx.into_js_object(&mut insp.ctx).unwrap();
        let (db, _db_guard) = EvmDbRef::new_changes(&result.state_changes, db);
        let db = db.into_js_object(&mut insp.ctx).unwrap();
        let result = insp
            .result_fn
            .call(&(insp.obj.clone().into()), &[ctx.into(), db.into()], &mut insp.ctx)
            .unwrap();
        to_serde_value(result, &mut insp.ctx).unwrap()
    }

    fn run_trace(code: &str, contract: Option<Bytes>, success: bool) -> serde_json::Value {
        let addr = Address::repeat_byte(0x01);
        let mut db = CacheDB::new(EmptyDB::default());

        db.insert_account_info(
            &Address::ZERO,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_000_000_000u64)),
        );
        db.insert_account_info(
            &addr,
            AccountInfo::default().with_code(Bytecode::new_legacy(
                contract.unwrap_or_else(|| hex!("6001600100").into()),
            )),
        );

        let insp = Rc::new(RefCell::new(
            JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap(),
        ));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            evm2::env::BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            db,
            Precompiles::base(SpecId::CANCUN),
        );
        evm.set_inspector(SharedJsInspector(Rc::clone(&insp)));

        let gas_price = 1024;
        let res = evm
            .transact(&RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
                TxLegacy {
                    gas_price,
                    gas_limit: 1_000_000,
                    to: TxKind::Call(addr),
                    ..Default::default()
                },
                Address::ZERO,
            )))
            .expect("pass without error");

        assert_eq!(res.status, success);
        js_result(&mut insp.borrow_mut(), &res, addr, gas_price, evm.database_mut())
    }

    #[test]
    fn test_loop_iteration_limit() {
        let mut context = Context::default();
        context.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);

        let code = "let i = 0; while (i++ < 69) {}";
        let result = context.eval(Source::from_bytes(code));
        assert!(result.is_ok());

        let code = "while (true) {}";
        let result = context.eval(Source::from_bytes(code));
        assert!(result.is_err());
    }

    #[test]
    fn test_fault_fn_not_callable() {
        let code = r#"
        {
            result: function() {},
            fault: {},
        }
    "#;
        let config = serde_json::Value::Null;
        let result = JsInspector::new(code.to_string(), config);
        assert!(matches!(result, Err(JsInspectorError::FaultFunctionMissing)));
    }

    #[test]
    fn test_general_counting() {
        let code = r#"{
        count: 0,
        step: function() { this.count += 1; },
        fault: function() {},
        result: function() { return this.count; }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res.as_u64().unwrap(), 3);
    }

    #[test]
    fn test_memory_access() {
        let code = r#"{
        depths: [],
        step: function(log) { this.depths.push(log.memory.slice(-1,-2)); },
        fault: function() {},
        result: function() { return this.depths; }
    }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_stack_peek() {
        let code = r#"{
        depths: [],
        step: function(log) { this.depths.push(log.stack.peek(-1)); },
        fault: function() {},
        result: function() { return this.depths; }
    }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_memory_get_uint() {
        let code = r#"{
        depths: [],
        step: function(log, db) { this.depths.push(log.memory.getUint(-64)); },
        fault: function() {},
        result: function() { return this.depths; }
    }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_stack_depth() {
        let code = r#"{
        depths: [],
        step: function(log) { this.depths.push(log.stack.length()); },
        fault: function() {},
        result: function() { return this.depths; }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res, json!([0, 1, 2]));
    }

    #[test]
    fn test_memory_length() {
        let code = r#"{
        lengths: [],
        step: function(log) { this.lengths.push(log.memory.length()); },
        fault: function() {},
        result: function() { return this.lengths; }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res, json!([0, 0, 0]));
    }

    #[test]
    fn test_opcode_to_string() {
        let code = r#"{
         opcodes: [],
         step: function(log) { this.opcodes.push(log.op.toString()); },
         fault: function() {},
         result: function() { return this.opcodes; }
     }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res, json!(["PUSH1", "PUSH1", "STOP"]));
    }

    #[test]
    fn test_gas_used() {
        let code = r#"{
        depths: [],
        step: function() {},
        fault: function() {},
        result: function(ctx) { return ctx.gasPrice+'.'+ctx.gasUsed; }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res.as_str().unwrap(), "1024.21006");
    }

    #[test]
    fn test_to_word() {
        let code = r#"{
        res: null,
        step: function(log) {},
        fault: function() {},
        result: function() { return toWord('0xffaa') }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(
            res,
            json!({
                "0": 0, "1": 0, "2": 0, "3": 0, "4": 0, "5": 0, "6": 0, "7": 0, "8": 0,
                "9": 0, "10": 0, "11": 0, "12": 0, "13": 0, "14": 0, "15": 0, "16": 0,
                "17": 0, "18": 0, "19": 0, "20": 0, "21": 0, "22": 0, "23": 0, "24": 0,
                "25": 0, "26": 0, "27": 0, "28": 0, "29": 0, "30": 255, "31": 170,
            })
        );
    }

    #[test]
    fn test_to_address() {
        let code = r#"{
        res: null,
        step: function(log) { var address = log.contract.getAddress(); this.res = toAddress(address); },
        fault: function() {},
        result: function() { return toHex(this.res) }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res.as_str().unwrap(), "0x0101010101010101010101010101010101010101");
    }

    #[test]
    fn test_to_address_string() {
        let code = r#"{
        res: null,
        step: function(log) { var address = '0x0000000000000000000000000000000000000000'; this.res = toAddress(address); },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res.as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), 0);
    }

    #[test]
    fn test_memory_slice() {
        let code = r#"{
        res: [],
        step: function(log) {
            var op = log.op.toString();
            if (op === 'MSTORE8' || op === 'STOP') {
                this.res.push(log.memory.slice(0, 2))
            }
        },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let contract = hex!("60ff60005300");
        let res = run_trace(code, Some(contract.into()), false);
        assert_eq!(res, json!([]));
    }

    #[test]
    fn test_memory_limit() {
        let code = r#"{
        res: [],
        step: function(log) { if (log.op.toString() === 'STOP') { this.res.push(log.memory.slice(5, 1025 * 1024)) } },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res, json!([]));
    }

    #[test]
    fn test_coinbase() {
        let code = r#"{
        lengths: [],
        step: function(log) { },
        fault: function() {},
        result: function(ctx) { var coinbase = ctx.coinbase; return toAddress(coinbase); }
    }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res.as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), 0);
    }

    #[test]
    fn test_individual_opcode_costs() {
        let code = r#"{
        res: [],
        step: function(log) {
            this.res.push(log.getCost());
        },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, None, true);

        assert_eq!(
            res.as_array().unwrap().iter().map(|v| v.as_u64().unwrap_or(0)).collect::<Vec<u64>>(),
            vec![0, 3, 3]
        );
    }

    #[test]
    fn test_slice_builtin() {
        let code = r#"{
        res: [],
        step: function(log) {
            var hex = '0xdeadbeefcafe';
            this.res.push(toHex(slice(hex, 0, 2)));
            this.res.push(toHex(slice(hex, 2, 4)));
            this.res.push(toHex(slice(hex, 4, 6)));

            var arr = [0x01, 0x02, 0x03, 0x04, 0x05];
            this.res.push(toHex(slice(arr, 0, 3)));
            this.res.push(toHex(slice(arr, 1, 4)));

            var uint8 = new Uint8Array([0xff, 0xee, 0xdd, 0xcc, 0xbb]);
            this.res.push(toHex(slice(uint8, 0, 2)));
            this.res.push(toHex(slice(uint8, 2, 5)));
        },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, Some(Bytes::from_static(&[0x00])), true);
        assert_eq!(
            res,
            json!(["0xdead", "0xbeef", "0xcafe", "0x010203", "0x020304", "0xffee", "0xddccbb"])
        );
    }

    #[test]
    fn test_is_precompiled_builtin() {
        let code = r#"{
        res: [],
        step: function(log) {
            this.res.push(isPrecompiled("0x01"));
            this.res.push(isPrecompiled("0x0000000000000000000000000000000000000002"));
            this.res.push(isPrecompiled("0x0000000000000000000000000000000000000000"));
        },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, Some(Bytes::from_static(&[0x00])), true);
        assert_eq!(res, json!([true, true, false]));
    }

    #[test]
    fn test_has_own_property() {
        let code = r#"{
        res: [],
        step: function(log) {
            this.res.push(log.hasOwnProperty("stack"));
        },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, Some(Bytes::from_static(&[0x00])), true);
        assert_eq!(res, json!([true]));
    }

    #[test]
    fn test_slice_with_stack_values() {
        let code = r#"{
        res: [],
        step: function(log) {
            if ((log.stack.length() > 0) && log.memory.length() >= log.stack.peek(0)) {
                this.res.push(log.memory.slice(0, log.stack.peek(0)));
            }
        },
        fault: function() {},
        result: function() { return this.res }
    }"#;
        let res = run_trace(code, Some(hex!("5F5F52600100").into()), true);
        assert_eq!(res, json!([json!({}), json!({}), json!({"0": 0})]));
    }
}
