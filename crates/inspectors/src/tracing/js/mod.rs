//! Javascript inspector

use crate::tracing::{
    TransactionContext,
    config::TraceStyle,
    js::{
        bindings::{
            CallFrame, Contract, EvmDbRef, FrameResult, JsEvmContext, MemoryRef, MemorySnapshot,
            ReusableCallFrame, ReusableEvmDb, ReusableFrameResult, ReusableStepLog, StackRef,
            StepLog,
        },
        builtins::{PrecompileList, register_builtins, to_serde_value},
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
use boa_engine::{Context, JsError, JsObject, JsResult, JsValue, Source, js_string};
use evm2::{
    Evm, EvmTypes, Inspector, TxResult,
    env::BlockEnv,
    ethereum::RecoveredTxEnvelope,
    evm::DynDatabase,
    interpreter::{
        GasTracker, InstrStop, Interpreter, Message, MessageKind, MessageResult, Word,
        opcode::OpCode,
    },
};

pub(crate) mod bindings;
pub(crate) mod builtins;

/// The maximum number of iterations in a loop.
///
/// Once exceeded, the loop will throw an error.
// An empty loop with this limit takes around 50ms to fail.
pub const LOOP_ITERATION_LIMIT: u64 = 200_000;

/// The recursion limit for function calls.
///
/// Once exceeded, the function will throw an error.
pub const RECURSION_LIMIT: usize = 10_000;

/// Pre-execution state captured in `step()` to be used in `step_end()`.
///
/// The JS step callback needs the pre-execution stack/memory state but the post-execution gas
/// cost. This struct holds the snapshot from `step()` so `step_end()` can invoke the callback
/// with the correct gas cost.
#[derive(Debug)]
struct PendingStep {
    /// Cloned stack from before opcode execution
    stack: Vec<Word>,
    /// Program counter
    pc: u64,
    /// Opcode being executed
    op: u8,
    /// Gas remaining before execution
    gas_remaining: u64,
    /// Call depth
    depth: u64,
    /// Gas refund counter
    refund: u64,
    /// Contract info
    contract: Contract,
    /// Total gas spent before this opcode (to compute delta in step_end)
    gas_spent_before: u64,
}

/// A javascript inspector that will delegate inspector functions to javascript functions
///
/// See also <https://geth.ethereum.org/docs/developers/evm-tracing/custom-tracer#custom-javascript-tracing>
#[derive(Debug)]
pub struct JsInspector {
    ctx: Context,
    /// The original javascript code used to create this inspector.
    code: String,
    /// The javascript config provided to the inspector.
    _js_config_value: JsValue,
    /// The input config object.
    config: serde_json::Value,
    /// The evaluated object that contains the inspector functions.
    obj: JsObject,
    /// The context of the transaction that is being inspected.
    transaction_context: TransactionContext,

    /// The javascript function that will be called when the result is requested.
    result_fn: JsObject,
    fault_fn: JsObject,

    // EVM inspector hook functions
    /// Invoked when the EVM enters a new call that is _NOT_ the top level call.
    ///
    /// Corresponds to [Inspector::call] and [Inspector::create_end] but is also invoked on
    /// [Inspector::selfdestruct].
    enter_fn: Option<JsObject>,
    /// Invoked when the EVM exits a call that is _NOT_ the top level call.
    ///
    /// Corresponds to [Inspector::call_end] and [Inspector::create_end] but also invoked after
    /// selfdestruct.
    exit_fn: Option<JsObject>,
    /// Executed before each instruction is executed.
    step_fn: Option<JsObject>,
    /// Reused step wrapper to avoid rebuilding the JS object graph per opcode.
    reusable_step_log: ReusableStepLog,
    /// Reused frame wrapper to avoid rebuilding the JS object graph per enter callback.
    reusable_call_frame: ReusableCallFrame,
    /// Reused frame wrapper to avoid rebuilding the JS object graph per exit callback.
    reusable_frame_result: ReusableFrameResult,
    /// Reused database wrapper shared by all callbacks.
    reusable_db: ReusableEvmDb,
    /// Keeps track of the current call stack.
    call_stack: Vec<CallStackItem>,
    /// Marker to track whether the precompiles have been registered.
    precompiles_registered: bool,
    /// Pre-execution state captured in `step()` to be processed in `step_end()`.
    pending_step: Option<PendingStep>,
    /// Cached memory snapshot, only updated when the previous opcode modifies memory.
    cached_memory: MemorySnapshot,
    /// The opcode from the previous step, used to decide whether to re-snapshot memory.
    prev_op: Option<OpCode>,
}

impl JsInspector {
    /// Creates a new inspector from a javascript code snipped that evaluates to an object with the
    /// expected fields and a config object.
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
    ///
    /// This also accepts a sender half of a channel to communicate with the database service so the
    /// DB can be queried from inside the inspector.
    pub fn new(code: String, config: serde_json::Value) -> Result<Self, JsInspectorError> {
        Self::with_transaction_context(code, config, Default::default())
    }

    /// Creates a new inspector from a javascript code snippet. See also [Self::new].
    ///
    /// This also accepts a [TransactionContext] that gives the JS code access to some contextual
    /// transaction infos.
    pub fn with_transaction_context(
        code: String,
        config: serde_json::Value,
        transaction_context: TransactionContext,
    ) -> Result<Self, JsInspectorError> {
        // Instantiate the execution context
        let mut ctx = Context::default();

        // Apply the default runtime limits
        // This is a safe guard to prevent infinite loops
        ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
        ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);

        register_builtins(&mut ctx)?;

        // evaluate the code
        let wrapped = format!("({code})");
        let obj =
            ctx.eval(Source::from_bytes(wrapped.as_bytes())).map_err(JsInspectorError::EvalCode)?;

        let obj = obj.as_object().ok_or(JsInspectorError::ExpectedJsObject)?;

        // ensure all the fields are callables, if present

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

            // call setup()
            setup_fn
                .call(&(obj.clone().into()), core::slice::from_ref(&_js_config_value), &mut ctx)
                .map_err(JsInspectorError::SetupCallFailed)?;
        }

        let reusable_step_log =
            ReusableStepLog::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_call_frame =
            ReusableCallFrame::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_frame_result =
            ReusableFrameResult::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_db = ReusableEvmDb::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;

        Ok(Self {
            ctx,
            code,
            _js_config_value,
            config,
            obj,
            transaction_context,
            result_fn,
            fault_fn,
            enter_fn,
            exit_fn,
            step_fn,
            reusable_step_log,
            reusable_call_frame,
            reusable_frame_result,
            reusable_db,
            call_stack: Default::default(),
            precompiles_registered: false,
            pending_step: None,
            cached_memory: MemorySnapshot::default(),
            prev_op: None,
        })
    }

    /// Returns the config object.
    pub const fn config(&self) -> &serde_json::Value {
        &self.config
    }

    /// Creates a fresh inspector from the same code and config, resetting all execution state.
    pub fn try_clone(&self) -> Result<Self, JsInspectorError> {
        Self::new(self.code.clone(), self.config.clone())
    }

    /// Returns the transaction context.
    pub const fn transaction_context(&self) -> &TransactionContext {
        &self.transaction_context
    }

    /// Sets the transaction context.
    pub const fn set_transaction_context(&mut self, transaction_context: TransactionContext) {
        self.transaction_context = transaction_context;
    }

    /// Applies the runtime limits to the JS context.
    ///
    /// By default
    pub fn set_runtime_limits(&mut self, limits: RuntimeLimits) {
        self.ctx.set_runtime_limits(limits);
    }

    /// Calls the result function and returns the result as [serde_json::Value].
    ///
    /// Note: This is supposed to be called after the inspection has finished.
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
        let (db, _db_guard) = EvmDbRef::new_changes(&result.state_changes, db);

        let mut to = None;
        let mut output_bytes = None;
        let mut error = None;

        if result.status {
            to = result.created_address;
            output_bytes = Some(result.output.clone());
        } else if result.stop.is_revert() {
            error = Some("execution reverted".to_string());
            output_bytes = Some(result.output.clone());
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
            output: output_bytes.unwrap_or_default(),
            time: block.timestamp.to_string(),
            intrinsic_gas: 0,
            transaction_ctx: self.transaction_context,
            error,
        };
        let ctx = ctx.into_js_object(&mut self.ctx)?;
        let db = db.into_js_object(&mut self.ctx)?;
        Ok(self.result_fn.call(
            &(self.obj.clone().into()),
            &[ctx.into(), db.into()],
            &mut self.ctx,
        )?)
    }

    fn try_fault(&mut self, step: StepLog, db: EvmDbRef) -> JsResult<()> {
        self.reusable_step_log.update(step);
        self.reusable_db.update(db);
        let step = self.reusable_step_log.value();
        let db = self.reusable_db.value();
        self.fault_fn.call(&(self.obj.clone().into()), &[step, db], &mut self.ctx)?;
        Ok(())
    }

    fn try_step(&mut self, step: StepLog, db: EvmDbRef) -> JsResult<()> {
        if let Some(step_fn) = &self.step_fn {
            self.reusable_step_log.update(step);
            self.reusable_db.update(db);
            let step = self.reusable_step_log.value();
            let db = self.reusable_db.value();
            step_fn.call(&(self.obj.clone().into()), &[step, db], &mut self.ctx)?;
        }
        Ok(())
    }

    fn try_enter(&mut self, frame: CallFrame) -> JsResult<()> {
        if let Some(enter_fn) = &self.enter_fn {
            self.reusable_call_frame.update(frame);
            enter_fn.call(
                &(self.obj.clone().into()),
                &[self.reusable_call_frame.value()],
                &mut self.ctx,
            )?;
        }
        Ok(())
    }

    fn try_exit(&mut self, frame: FrameResult) -> JsResult<()> {
        if let Some(exit_fn) = &self.exit_fn {
            self.reusable_frame_result.update(frame);
            exit_fn.call(
                &(self.obj.clone().into()),
                &[self.reusable_frame_result.value()],
                &mut self.ctx,
            )?;
        }
        Ok(())
    }

    /// Returns the currently active call
    ///
    /// Panics: if there's no call yet
    #[track_caller]
    fn active_call(&self) -> &CallStackItem {
        self.call_stack.last().expect("call stack is empty")
    }

    #[inline]
    fn pop_call(&mut self) {
        self.call_stack.pop();
    }

    /// Returns true whether the active call is the root call.
    #[inline]
    const fn is_root_call_active(&self) -> bool {
        self.call_stack.len() == 1
    }

    /// Returns true if there's an enter function and the active call is not the root call.
    #[inline]
    const fn can_call_enter(&self) -> bool {
        self.enter_fn.is_some() && !self.is_root_call_active()
    }

    /// Returns true if there's an exit function and the active call is not the root call.
    #[inline]
    const fn can_call_exit(&mut self) -> bool {
        self.exit_fn.is_some() && !self.is_root_call_active()
    }

    /// Pushes a new call to the stack
    fn push_call(
        &mut self,
        contract: Address,
        input: Bytes,
        value: U256,
        kind: CallKind,
        caller: Address,
        gas_limit: u64,
    ) -> &CallStackItem {
        let call = CallStackItem {
            contract: Contract { caller, contract, value, input },
            kind,
            gas_limit,
        };
        self.call_stack.push(call);
        self.active_call()
    }

    /// Registers the precompiles in the JS context
    fn register_precompiles<T: EvmTypes<Host = Evm<T>>>(&mut self, host: &Evm<T>) {
        if self.precompiles_registered {
            return;
        }
        let precompiles = PrecompileList(HashSet::from_iter(host.precompiles().addresses()));

        let _ = precompiles.register_callable(&mut self.ctx);

        self.precompiles_registered = true
    }
}

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for JsInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, T>) {
        if self.step_fn.is_none() {
            return;
        }

        // Update the cached memory snapshot only if the previous opcode modified memory.
        // This avoids an expensive Vec<u8> clone on every single step.
        let should_update_memory = self.prev_op.is_none_or(|prev| prev.modifies_memory());
        if should_update_memory {
            self.cached_memory = MemorySnapshot::new(interp.memory_ref());
        }

        let op = interp.opcode();
        self.prev_op = OpCode::new(op);

        let active_call = self.active_call();
        let message = interp.message();
        self.pending_step = Some(PendingStep {
            stack: interp.stack().as_slice().to_vec(),
            pc: interp.pc() as u64,
            op,
            gas_remaining: interp.gas().remaining(),
            depth: u64::from(message.depth),
            refund: interp.gas().refunded() as u64,
            contract: Contract {
                caller: message.caller,
                contract: message.destination,
                value: active_call.contract.value,
                input: active_call.contract.input.clone(),
            },
            gas_spent_before: interp.gas().spent(),
        });
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>) {
        if self.step_fn.is_none() {
            return;
        }

        let Some(pending) = self.pending_step.take() else {
            return;
        };

        let result = interp.result();
        let is_revert = matches!(result, Err(stop) if stop.is_revert());
        let cost = interp.gas().spent().saturating_sub(pending.gas_spent_before);

        let (db, db_guard) = EvmDbRef::new_state(interp.host().state_mut());
        let (stack, stack_guard) = StackRef::new_owned(pending.stack);
        let (memory, memory_guard) = MemoryRef::new_owned(self.cached_memory.clone());

        let stop = if is_revert {
            let step = StepLog {
                stack,
                op: OpCode::REVERT.get().into(),
                pc: pending.pc,
                memory,
                gas_remaining: pending.gas_remaining,
                cost,
                depth: pending.depth,
                refund: pending.refund,
                error: result.err().map(|err| format!("{err:?}")),
                contract: pending.contract,
            };

            let _ = self.try_fault(step, db);
            false
        } else {
            let step = StepLog {
                stack,
                op: pending.op.into(),
                memory,
                pc: pending.pc,
                gas_remaining: pending.gas_remaining,
                cost,
                depth: pending.depth,
                refund: pending.refund,
                error: None,
                contract: pending.contract,
            };

            self.try_step(step, db).is_err() && result.is_ok()
        };

        drop(memory_guard);
        drop(stack_guard);
        drop(db_guard);

        if stop {
            interp.set_stop(InstrStop::Revert);
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.register_precompiles(interp.host());

        // determine contract and caller based on the call scheme
        let (caller, contract) = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => {
                (message.destination, message.code_address)
            }
            _ => (message.caller, message.destination),
        };

        let value =
            if message.kind == MessageKind::DelegateCall { U256::ZERO } else { message.value };
        self.push_call(
            contract,
            message.input.clone(),
            value,
            message.kind.into(),
            caller,
            message.gas_limit,
        );

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
        _interp: &mut Interpreter<'_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
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

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.register_precompiles(interp.host());
        self.push_call(
            message.destination,
            message.input.clone(),
            message.value,
            message.kind.into(),
            message.caller,
            message.gas_limit,
        );

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
        _interp: &mut Interpreter<'_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
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
        // This is exempt from the root call constraint, because selfdestruct is treated as a
        // new scope that is entered and immediately exited.
        if self.enter_fn.is_some() {
            let call = self.active_call();
            let frame =
                CallFrame { contract: call.contract.clone(), kind: call.kind, gas: call.gas_limit };
            let _ = self.try_enter(frame);
        }

        // exit with empty frame result ref <https://github.com/ethereum/go-ethereum/blob/0004c6b229b787281760b14fb9460ffd9c2496f1/core/vm/instructions.go#L829-L829>
        if self.exit_fn.is_some() {
            let frame_result = FrameResult { gas_used: 0, output: Bytes::new(), error: None };
            let _ = self.try_exit(frame_result);
        }
    }
}

/// Represents an active call
#[derive(Debug)]
struct CallStackItem {
    contract: Contract,
    kind: CallKind,
    gas_limit: u64,
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

/// Converts a JavaScript error into a [InstrStop::Revert] [MessageResult].
#[inline]
fn js_error_to_revert<T: EvmTypes>(err: JsError) -> MessageResult<T> {
    let output = err.to_string().as_bytes().to_vec();
    MessageResult {
        stop: InstrStop::Revert,
        output: output.into(),
        gas: GasTracker::new(0),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, TxKind, U256, bytes, hex};
    use evm2::{
        BaseEvmTypes, Evm, Precompiles, SpecId,
        bytecode::Bytecode,
        ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, CacheDB, EmptyDB},
        interpreter::Host,
    };
    use serde_json::json;

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

    // Helper function to run a trace and return the result
    fn run_trace(code: &str, contract: Option<Bytes>, success: bool) -> serde_json::Value {
        let addr = Address::repeat_byte(0x01);
        let mut db = CacheDB::new(EmptyDB::default());

        // Insert the caller
        db.insert_account_info(
            &Address::ZERO,
            AccountInfo { balance: U256::from(1e18), ..Default::default() },
        );
        // Insert the contract
        db.insert_account_info(
            &addr,
            AccountInfo {
                code: Some(Bytecode::new_legacy(
                    /* PUSH1 1, PUSH1 1, STOP */
                    contract.unwrap_or_else(|| hex!("6001600100").into()),
                )),
                ..Default::default()
            },
        );

        let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            evm2::env::BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            db,
            Precompiles::base(SpecId::CANCUN),
        );
        evm.set_inspector(insp);

        let tx = RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(
            TxLegacy {
                gas_price: 1024,
                gas_limit: 1_000_000,
                to: TxKind::Call(addr),
                ..Default::default()
            },
            Address::ZERO,
        ));
        let res = evm.transact(&tx).expect("pass without error");

        assert_eq!(res.status, success);
        let mut inspector = evm.clear_inspector_as::<JsInspector>().unwrap();
        let block = *evm.block_env();
        inspector.json_result(&res, &tx, &block, evm.database_mut()).unwrap()
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
    fn test_memory_slice_rejects_non_finite_indexes() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.memory.slice(Infinity, NaN)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_memory_slice_rejects_non_finite_end() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.memory.slice(0, Infinity)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_memory_slice_accepts_bigint_index() {
        let code = r#"{
            res: [],
            step: function(log) { this.res.push(log.memory.slice(0, 0n)); },
            fault: function() {},
            result: function() { return this.res; }
        }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res, json!([json!({}), json!({}), json!({})]));
    }

    #[test]
    fn test_memory_slice_rejects_bigint_index_overflow() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.memory.slice(0, 340282366920938463463374607431768211455n)); },
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
    fn test_stack_peek_nan() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.stack.peek(NaN)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_stack_peek_infinity() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.stack.peek(Infinity)); },
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
    fn test_memory_get_uint_rejects_non_finite_offset() {
        let code = r#"{
            depths: [],
            step: function(log, db) { this.depths.push(log.memory.getUint(Infinity)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let res = run_trace(code, None, false);
        assert_eq!(res.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_memory_get_uint_rejects_nan_offset() {
        let code = r#"{
            depths: [],
            step: function(log, db) { this.depths.push(log.memory.getUint(NaN)); },
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
        let contract = hex!("60ff60005300"); // PUSH1, 0xff, PUSH1, 0x00, MSTORE8, STOP
        let res = run_trace(code, Some(contract.into()), false);
        assert_eq!(res, json!([]));
    }

    #[test]
    fn test_memory_limit() {
        // Accessing out-of-bounds memory in the tracer results in an empty array.
        // Since we invoke the JS step callback in step_end (after the opcode executes),
        // a JS error on the final STOP opcode cannot revert the transaction — it has
        // already completed. The transaction succeeds but the trace result is empty.
        let code = r#"{
            res: [],
            step: function(log) { if (log.op.toString() === 'STOP') { this.res.push(log.memory.slice(5, 1025 * 1024)) } },
            fault: function() {},
            result: function() { return this.res }
        }"#;
        let res = run_trace(code, None, true);
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

        // The bytecode is: PUSH1 0x01, PUSH1 0x01, STOP
        // Expected costs: PUSH1=3, PUSH1=3, STOP=0
        assert_eq!(
            res.as_array().unwrap().iter().map(|v| v.as_u64().unwrap_or(0)).collect::<Vec<u64>>(),
            vec![3, 3, 0]
        );
    }

    #[test]
    fn test_slice_builtin() {
        let code = r#"{
            res: [],
            step: function(log) {
                // Test slicing a hex string
                var hex = '0xdeadbeefcafe';
                this.res.push(toHex(slice(hex, 0, 2)));
                this.res.push(toHex(slice(hex, 2, 4)));
                this.res.push(toHex(slice(hex, 4, 6)));

                // Test slicing an array
                var arr = [0x01, 0x02, 0x03, 0x04, 0x05];
                this.res.push(toHex(slice(arr, 0, 3)));
                this.res.push(toHex(slice(arr, 1, 4)));

                // Test slicing a Uint8Array
                var uint8 = new Uint8Array([0xff, 0xee, 0xdd, 0xcc, 0xbb]);
                this.res.push(toHex(slice(uint8, 0, 2)));
                this.res.push(toHex(slice(uint8, 2, 5)));
            },
            fault: function() {},
            result: function() { return this.res }
        }"#;
        let res = run_trace(code, Some(bytes!("0x00")), true);
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
        let res = run_trace(code, Some(bytes!("0x00")), true);
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
        let res = run_trace(code, Some(bytes!("0x00")), true);
        assert_eq!(res, json!([true]));
    }

    #[test]
    fn test_step_reuses_log_and_db_objects() {
        let code = r#"{
            prevLog: null,
            prevDb: null,
            sameLog: [],
            sameDb: [],
            step: function(log, db) {
                if (this.prevLog !== null) {
                    this.sameLog.push(this.prevLog === log);
                }
                if (this.prevDb !== null) {
                    this.sameDb.push(this.prevDb === db);
                }
                this.prevLog = log;
                this.prevDb = db;
            },
            fault: function() {},
            result: function() {
                return {
                    sameLog: this.sameLog,
                    sameDb: this.sameDb,
                };
            }
        }"#;
        let res = run_trace(code, None, true);
        assert_eq!(res, json!({ "sameLog": [true, true], "sameDb": [true, true] }));
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
        let res = run_trace(code, Some(bytes!("0x5F5F52600100")), true);
        assert_eq!(res, json!([json!({}), json!({}), json!({"0": 0})]));
    }

    #[test]
    fn test_bigint_survives_poisoned_global() {
        let code = r#"{
            res: {},
            step: function(log, db) {
                // Poison the global bigint alias
                Object.defineProperty(globalThis, 'bigint', {
                    get() { throw new Error('poisoned bigint'); },
                    configurable: true
                });

                if (log.stack.length() > 0) {
                    // stack.peek internally uses to_bigint
                    this.res.stackPeek = log.stack.peek(0).toString();
                }
                // contract.getValue internally uses to_bigint
                this.res.value = log.contract.getValue().toString();
                // db.getBalance internally uses to_bigint
                this.res.balance = db.getBalance(log.contract.getAddress()).toString();
            },
            fault: function() {},
            result: function() { return this.res }
        }"#;
        let res = run_trace(code, None, true);
        let obj = res.as_object().unwrap();
        assert_eq!(obj["stackPeek"], json!("1"));
        assert_eq!(obj["value"], json!("0"));
        assert_eq!(obj["balance"], json!("0"));
    }
}
