//! Javascript inspector

use crate::tracing::{
    TransactionContext,
    config::TraceStyle,
    js::{
        bindings::{
            CallFrame, Contract, FrameKind, FrameResult, JsEvmContext, OpcodeNames, PreStep,
            ReusableCallFrame, ReusableEvmDb, ReusableFrameResult, ReusableStepLog, StepInfo,
            StepSnapshot,
        },
        builtins::{PrecompileList, register_builtins, to_serde_value},
    },
    tx_state::TxState,
    types::CallKind,
    utils,
};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use alloy_consensus::{Transaction, transaction::Recovered};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsValue, Script, Source,
    builtins::error::Error, error::EngineError, js_string, object::builtins::JsArray,
    property::PropertyDescriptor,
};
use core::sync::atomic::{AtomicBool, Ordering};
use evm2::{
    Evm, EvmTypes, EvmTypesHost, ExecutionError, Inspector, TxResultWithState,
    env::BlockEnv,
    evm::DynDatabase,
    interpreter::{InstrStop, Interpreter, Message, MessageKind, MessageResult, opcode::op},
};

pub use boa_engine::vm::RuntimeLimits;

pub(crate) mod bindings;
pub(crate) mod builtins;

#[cfg(test)]
mod snapshot_tests;

#[cfg(test)]
mod interrupt_tests;

/// The maximum number of iterations in a loop.
///
/// Once exceeded, the loop will throw an error.
// An empty loop with this limit takes around 50ms to fail.
pub const LOOP_ITERATION_LIMIT: u64 = 200_000;

/// The recursion limit for function calls.
///
/// Once exceeded, the function will throw an error.
/// Kept below Boa's default because recursive accessors also consume native stack space.
pub const RECURSION_LIMIT: usize = 128;

/// Reused pre-execution data for one call depth. Parents remain pending during child execution.
#[derive(Debug, Default)]
struct PendingStep {
    snapshot: StepSnapshot,
    gas_spent_before: u64,
    frame_refund: i64,
    instruction: FaultStep,
    active: bool,
}

/// A javascript inspector that will delegate inspector functions to javascript functions
///
/// See also <https://geth.ethereum.org/docs/developers/evm-tracing/custom-tracer#custom-javascript-tracing>
pub struct JsInspector {
    ctx: Context,
    /// The original javascript code used to create this inspector.
    code: String,
    /// The parsed tracer script, evaluated again by [`Self::fuse`].
    script: Script,
    /// The input config object.
    config: serde_json::Value,
    /// The evaluated object that contains the inspector functions.
    obj: JsObject,
    /// Cached `this` value for callbacks.
    this: JsValue,
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
    /// Lazily initialized opcode names shared across transaction resets.
    op_names: OpcodeNames,
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
    /// Monotonically increasing ID used to refresh callback contract input.
    next_call_id: u64,
    /// The precompiles read by `isPrecompiled`, set on the first call hook of a transaction.
    precompiles: PrecompileList,
    /// Pre-execution states captured in `step()` to be processed in `step_end()`.
    pending_steps: Vec<PendingStep>,
    /// Depth of the root frame, which can be non-zero for a nested transaction root.
    ///
    /// Pending steps are indexed relative to it.
    root_depth: u16,
    /// Retained until `fuse`; result collection prioritizes it over cancellation.
    hook_error: Option<JsError>,
    /// Optional cancellation signal shared with the caller.
    interrupt: Option<JsInspectorInterrupt>,
}

impl core::fmt::Debug for JsInspector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JsInspector")
            .field("code", &self.code)
            .field("config", &self.config)
            .field("transaction_context", &self.transaction_context)
            .field("call_stack", &self.call_stack)
            .finish_non_exhaustive()
    }
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
    /// - `setup`: a function that will be called before the inspection starts, with the config as a
    ///   JSON string.
    /// - `enter` and `exit`: functions that will be called when the execution enters and exits a
    ///   call. Both must be present or both absent.
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
        let precompiles = PrecompileList::default();
        precompiles.register_callable(&mut ctx)?;

        // parse the code
        let wrapped = format!("({code})");
        let script = Script::parse(Source::from_bytes(wrapped.as_bytes()), None, &mut ctx)
            .map_err(JsInspectorError::EvalCode)?;

        let JsTracerObject { obj, result_fn, fault_fn, enter_fn, exit_fn, step_fn } =
            JsTracerObject::evaluate(&script, &config, &mut ctx)?;

        let op_names = OpcodeNames::new();
        let reusable_step_log =
            ReusableStepLog::new(&mut ctx, op_names.clone()).map_err(JsInspectorError::EvalCode)?;
        let reusable_call_frame =
            ReusableCallFrame::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_frame_result =
            ReusableFrameResult::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_db = ReusableEvmDb::new(&mut ctx).map_err(JsInspectorError::EvalCode)?;

        Ok(Self {
            ctx,
            code,
            script,
            config,
            this: obj.clone().into(),
            obj,
            transaction_context,
            result_fn,
            fault_fn,
            enter_fn,
            exit_fn,
            step_fn,
            op_names,
            reusable_step_log,
            reusable_call_frame,
            reusable_frame_result,
            reusable_db,
            call_stack: Default::default(),
            next_call_id: 1,
            precompiles,
            pending_steps: Vec::new(),
            root_depth: 0,
            hook_error: None,
            interrupt: None,
        })
    }

    /// Returns the config object.
    pub const fn config(&self) -> &serde_json::Value {
        &self.config
    }

    /// Creates a fresh inspector from the same code and config, sharing the interrupt flag.
    pub fn try_clone(&self) -> Result<Self, JsInspectorError> {
        self.ensure_not_interrupted()?;
        let mut inspector = Self::new(self.code.clone(), self.config.clone())?;
        inspector.interrupt = self.interrupt.clone();
        Ok(inspector)
    }

    /// Resets the inspector to its initial state so it can be used for the next transaction.
    ///
    /// This evaluates the tracer script again in the existing JS context, which yields a fresh
    /// tracer object (and invokes its `setup` function) without parsing the script again. Global
    /// state the previous tracer object may have modified, e.g. prototypes, is kept. Callback
    /// wrappers are recreated so their own properties do not carry over between transactions.
    pub fn fuse(&mut self) -> Result<(), JsInspectorError> {
        self.ensure_not_interrupted()?;
        // `setup` of the fresh tracer must not see the previous transaction's precompiles.
        self.precompiles.clear();
        let JsTracerObject { obj, result_fn, fault_fn, enter_fn, exit_fn, step_fn } =
            JsTracerObject::evaluate(&self.script, &self.config, &mut self.ctx)?;
        // Callback objects are mutable JS objects: replacing their Rust state does not remove
        // user-defined properties or restore overwritten methods. Rebuild them once per
        // transaction, while retaining the parsed script and reusing wrappers within a transaction.
        let reusable_step_log = ReusableStepLog::new(&mut self.ctx, self.op_names.clone())
            .map_err(JsInspectorError::EvalCode)?;
        let reusable_call_frame =
            ReusableCallFrame::new(&mut self.ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_frame_result =
            ReusableFrameResult::new(&mut self.ctx).map_err(JsInspectorError::EvalCode)?;
        let reusable_db = ReusableEvmDb::new(&mut self.ctx).map_err(JsInspectorError::EvalCode)?;

        self.reusable_step_log = reusable_step_log;
        self.reusable_call_frame = reusable_call_frame;
        self.reusable_frame_result = reusable_frame_result;
        self.reusable_db = reusable_db;
        self.this = obj.clone().into();
        self.obj = obj;
        self.result_fn = result_fn;
        self.fault_fn = fault_fn;
        self.enter_fn = enter_fn;
        self.exit_fn = exit_fn;
        self.step_fn = step_fn;
        self.call_stack.clear();
        self.pending_steps.clear();
        self.root_depth = 0;
        self.hook_error = None;

        Ok(())
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
    /// Increasing the recursion limit can overflow the native stack when tracing code uses
    /// recursive property accessors.
    pub fn set_runtime_limits(&mut self, limits: RuntimeLimits) {
        self.ctx.set_runtime_limits(limits);
    }

    /// Calls the result function and returns the result as [serde_json::Value].
    ///
    /// Note: This is supposed to be called after the inspection has finished.
    pub fn json_result<T: EvmTypesHost<Tx: Transaction>>(
        &mut self,
        res: &TxResultWithState<T>,
        tx: &Recovered<T::Tx>,
        block: &BlockEnv<T>,
        db: &mut dyn DynDatabase,
    ) -> Result<serde_json::Value, JsInspectorError> {
        let result = self.result(res, tx, block, db)?;
        let result = to_serde_value(result, &mut self.ctx)?;
        self.ensure_not_interrupted()?;
        Ok(result)
    }

    /// Calls the result function and returns the result.
    pub fn result<T: EvmTypesHost<Tx: Transaction>>(
        &mut self,
        res: &TxResultWithState<T>,
        tx: &Recovered<T::Tx>,
        block: &BlockEnv<T>,
        db: &mut dyn DynDatabase,
    ) -> Result<JsValue, JsInspectorError> {
        if let Some(err) = &self.hook_error {
            return Err(JsInspectorError::JsError(err.clone()));
        }
        self.ensure_not_interrupted()?;
        let TxResultWithState { result, pending_state, .. } = res;
        let state = TxState::from_pending(pending_state);

        let mut output_bytes = None;
        let mut error = None;

        if result.status {
            output_bytes = Some(result.output.clone());
        } else if result.stop.is_revert() {
            error = Some("execution reverted".to_string());
            output_bytes = Some(result.output.clone());
        } else {
            error = utils::fmt_error_msg(result.stop, TraceStyle::Geth);
        }

        let kind = tx.kind();
        let to = Some(match kind {
            TxKind::Call(target) => target,
            TxKind::Create => {
                result.created_address.unwrap_or_else(|| tx.signer().create(tx.nonce()))
            }
        });

        let base_fee = block.basefee.try_into().unwrap_or(u64::MAX);
        let gas_tip = tx.effective_tip_per_gas(base_fee).unwrap_or_default();

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
            gas_used: result.tx_gas_used(),
            gas_price: U256::from(gas_tip),
            value: tx.value(),
            block: block.number.try_into().unwrap_or(u64::MAX),
            coinbase: block.beneficiary,
            output: output_bytes.unwrap_or_default(),
            transaction_ctx: self.transaction_context,
            error,
        };
        let ctx = ctx.into_js_object(&mut self.ctx)?;
        let result = self.reusable_db.with_changes_scope(&state, db, || {
            self.result_fn.call(&self.this, &[ctx.into(), self.reusable_db.value()], &mut self.ctx)
        });
        self.ensure_not_interrupted()?;
        Ok(result?)
    }

    fn try_enter(&mut self, frame: CallFrame) -> JsResult<()> {
        if let Some(enter_fn) = &self.enter_fn {
            self.reusable_call_frame.update(frame);
            enter_fn.call(&self.this, &[self.reusable_call_frame.value()], &mut self.ctx)?;
        }
        Ok(())
    }

    fn try_exit(&mut self, frame: FrameResult) -> JsResult<()> {
        if let Some(exit_fn) = &self.exit_fn {
            self.reusable_frame_result.update(frame);
            exit_fn.call(&self.this, &[self.reusable_frame_result.value()], &mut self.ctx)?;
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
        self.enter_fn.is_some() && !self.is_root_call_active() && !self.hook_errored()
    }

    /// Returns true if there's an exit function and the active call is not the root call.
    #[inline]
    const fn can_call_exit(&mut self) -> bool {
        self.exit_fn.is_some() && !self.is_root_call_active() && !self.hook_errored()
    }

    /// Pushes a new call to the stack
    fn push_call(
        &mut self,
        depth: u16,
        contract: Contract,
        kind: CallKind,
        gas_limit: u64,
        frame_refund: i64,
    ) -> &CallStackItem {
        if self.call_stack.is_empty() {
            self.root_depth = depth;
        }
        let enclosing_refund =
            self.call_stack.last().map_or(0, |parent| parent.enclosing_refund + frame_refund);
        let call =
            CallStackItem { id: self.next_call_id, contract, kind, gas_limit, enclosing_refund };
        self.next_call_id += 1;
        self.call_stack.push(call);
        self.active_call()
    }

    /// Sets the active precompiles read by `isPrecompiled`.
    fn register_precompiles<T: EvmTypes>(&mut self, host: &Evm<'_, T>) {
        if !self.precompiles.is_set() {
            self.precompiles.set(host.precompiles().addresses());
        }
    }

    /// Sets a shared handle that cooperatively interrupts JavaScript tracing.
    ///
    /// Call [`JsInspectorInterrupt::interrupt`] from a timeout or cancellation handler to abort
    /// execution with a fatal evm2 error at the next inspection boundary. [`Self::try_clone`]
    /// shares the handle and [`Self::fuse`] does not reset it. Result collection also fails after
    /// interruption.
    ///
    /// The handle is checked even if the tracer has no `step` callback. Checking it performs a
    /// relaxed atomic load without cloning the handle. Without a handle, no atomic loads are
    /// performed.
    ///
    /// This cannot preempt a running JavaScript callback, native operation or precompile. It does
    /// not bound memory usage or interrupt script evaluation and `setup` in the constructor.
    /// Boa's runtime limits still apply while JavaScript is running.
    ///
    /// ```
    /// use evm2_inspectors::tracing::js::{JsInspector, JsInspectorInterrupt};
    ///
    /// let interrupt = JsInspectorInterrupt::new();
    /// let inspector = JsInspector::new(
    ///     "{ fault: function() {}, result: function() {} }".into(),
    ///     serde_json::Value::Null,
    /// )?
    /// .with_interrupt(interrupt.clone());
    /// // The request's timeout or cancellation handler can signal the blocking execution.
    /// interrupt.interrupt();
    /// # Ok::<(), evm2_inspectors::tracing::js::JsInspectorError>(())
    /// ```
    #[must_use]
    pub fn with_interrupt(mut self, interrupt: JsInspectorInterrupt) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    #[inline]
    fn is_interrupted(&self) -> bool {
        self.interrupt.as_ref().is_some_and(JsInspectorInterrupt::is_interrupted)
    }

    fn ensure_not_interrupted(&self) -> Result<(), JsInspectorError> {
        if self.is_interrupted() {
            return Err(JsInspectorError::Interrupted);
        }
        Ok(())
    }

    #[inline]
    fn check_interrupt<T: EvmTypes>(&self, interp: &mut Interpreter<'_, '_, T>) -> bool {
        if !self.is_interrupted() {
            return false;
        }
        // Preserve an existing fatal stop and its execution error.
        if !interp.result().is_err_and(|stop| stop.is_fatal()) {
            interp.fail(ExecutionError::Fatal(JsInspectorError::Interrupted.to_string().into()));
        }
        true
    }

    #[inline]
    fn check_and_halt<T: EvmTypes>(&mut self, interp: &mut Interpreter<'_, '_, T>) -> bool {
        if !self.check_interrupt(interp) {
            return false;
        }
        let index = self.pending_step_index(interp);
        if let Some(pending) = self.pending_steps.get_mut(index) {
            pending.active = false;
        }
        // Step hooks must set the stop as well as record the fatal execution error.
        interp.set_stop(InstrStop::FatalExternalError);
        true
    }

    fn record_hook_error(&mut self, hook: &'static str, err: JsError) {
        if self.hook_error.is_none() {
            let suffix = format!("    in server-side tracer function '{hook}'");
            self.hook_error = Some(with_message_suffix(err, &suffix, &mut self.ctx));
        }
    }

    const fn hook_errored(&self) -> bool {
        self.hook_error.is_some()
    }

    fn invoke_step_hook<T: EvmTypes>(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        fault: bool,
        error: Option<String>,
    ) {
        let depth = interp.message().depth;
        let index = self.pending_step_index(interp);
        let pending = &mut self.pending_steps[index];
        let call = self.call_stack.last().expect("call stack is empty");
        // evm2's CALL/CREATE step_end runs after the child has returned and merged its refund.
        let frame_refund = if matches!(
            pending.instruction.op,
            op::CALL | op::CALLCODE | op::DELEGATECALL | op::STATICCALL | op::CREATE | op::CREATE2
        ) {
            pending.frame_refund
        } else {
            interp.gas().refunded()
        };
        let refund = call.enclosing_refund + frame_refund;
        debug_assert!(refund >= 0, "transaction-wide refund counter went negative: {refund}");
        let info = StepInfo {
            cost: interp.gas().spent().saturating_sub(pending.gas_spent_before),
            refund: refund.max(0) as u64,
            // Geth reports the depth starting at 1 for the top-level call.
            depth: u64::from(depth) + 1,
            error,
            caller: interp.message().caller,
            contract: interp.message().destination,
            value: call.contract.value,
            input: &call.contract.input,
            call_id: call.id,
        };
        let (stack, memory, host) = interp.stack_memory_host();
        let res = self.reusable_db.with_state_scope(host.state_mut(), || {
            self.reusable_step_log.with_scope(
                &mut pending.snapshot,
                stack.as_slice(),
                memory,
                info,
                || {
                    let f = if fault { &self.fault_fn } else { self.step_fn.as_ref().unwrap() };
                    f.call(
                        &self.this,
                        &[self.reusable_step_log.value(), self.reusable_db.value()],
                        &mut self.ctx,
                    )
                },
            )
        });
        if let Err(err) = res {
            self.record_hook_error(if fault { "fault" } else { "step" }, err);
        }
    }

    /// Ends the parent's call-like step before its child runs, like geth.
    ///
    /// Invoked on [Inspector::call] and [Inspector::create], where the parent has charged the step,
    /// including the gas forwarded to the child, and the child has not returned any gas yet.
    /// `step_end` then skips the step. A `step` error is recorded like on any other step and does
    /// not affect the child.
    fn end_call_step<T: EvmTypes>(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
    ) {
        // The top-level message has no parent step.
        if self.step_fn.is_none()
            || self.hook_errored()
            || message.depth != interp.message().depth + 1
        {
            return;
        }
        let index = self.pending_step_index(interp);
        if let Some(pending) = self.pending_steps.get_mut(index)
            && core::mem::take(&mut pending.active)
        {
            self.invoke_step_hook(interp, false, None);
        }
    }

    /// Returns the pending-step slot of the current frame, relative to the root frame.
    #[inline]
    fn pending_step_index<T: EvmTypes>(&self, interp: &Interpreter<'_, '_, T>) -> usize {
        usize::from(interp.message().depth - self.root_depth)
    }
}

impl<T: EvmTypes> Inspector<T> for JsInspector {
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if self.check_and_halt(interp) || self.hook_errored() {
            return;
        }
        let index = self.pending_step_index(interp);
        if self.pending_steps.len() <= index {
            self.pending_steps.resize_with(index + 1, PendingStep::default);
        }
        let pending = &mut self.pending_steps[index];
        pending.gas_spent_before = interp.gas().spent();
        pending.frame_refund = interp.gas().refunded();
        pending.instruction = FaultStep {
            pc: interp.pc() as u64,
            op: interp.opcode(),
            gas: interp.gas().remaining(),
        };
        pending.active = true;
        if self.step_fn.is_some() {
            pending.snapshot.record(PreStep {
                pc: pending.instruction.pc,
                op: pending.instruction.op,
                gas_remaining: pending.instruction.gas,
                stack: interp.stack().as_slice(),
                memory: interp.memory().as_slice(),
            });
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if self.check_and_halt(interp) || self.hook_errored() {
            return;
        }
        let index = self.pending_step_index(interp);
        let Some(pending) = self.pending_steps.get_mut(index) else {
            return;
        };
        if !core::mem::take(&mut pending.active) {
            return;
        }
        let result = interp.result().err();
        let is_fault = result.is_some_and(|stop| should_fault(stop, pending.instruction.op));
        let error = result.and_then(|stop| utils::fmt_error_msg(stop, TraceStyle::Geth));
        if self.step_fn.is_none() {
            if !is_fault {
                return;
            }
            // Fault-only tracers expose post-execution stack and memory.
            pending.snapshot.record(PreStep {
                pc: pending.instruction.pc,
                op: pending.instruction.op,
                gas_remaining: pending.instruction.gas,
                stack: interp.stack().as_slice(),
                memory: interp.memory().as_slice(),
            });
        } else {
            self.invoke_step_hook(interp, false, if is_fault { None } else { error.clone() });
            if self.check_and_halt(interp) || self.hook_errored() {
                return;
            }
        }
        if is_fault {
            self.invoke_step_hook(interp, true, error);
            self.check_and_halt(interp);
        }
    }

    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        if self.check_interrupt(interp) {
            return None;
        }
        self.register_precompiles(interp.host());
        self.end_call_step(interp, message);

        // determine contract and caller based on the call scheme
        let (caller, contract) = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => {
                (message.destination, message.call_target)
            }
            _ => (message.caller, message.destination),
        };

        self.push_call(
            message.depth,
            Contract { caller, contract, value: message.value, input: message.input.clone() },
            message.kind.into(),
            message.gas_limit,
            interp.gas().refunded(),
        );

        if self.can_call_enter() {
            let call = self.active_call();
            let frame = CallFrame {
                contract: call.contract.clone(),
                kind: FrameKind::Call(call.kind),
                gas: call.gas_limit,
            };
            if let Err(err) = self.try_enter(frame) {
                self.record_hook_error("enter", err);
            }
        }

        self.check_interrupt(interp);
        None
    }

    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if self.check_interrupt(interp) {
            return;
        }
        if self.can_call_exit() {
            let frame_result = FrameResult {
                gas_used: result.gas.spent(),
                output: result.output.clone(),
                error: utils::fmt_error_msg(result.stop, TraceStyle::Geth),
            };
            if let Err(err) = self.try_exit(frame_result) {
                self.record_hook_error("exit", err);
            }
        }

        self.pop_call();
        self.check_interrupt(interp);
    }

    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        if self.check_interrupt(interp) {
            return None;
        }
        self.register_precompiles(interp.host());
        self.end_call_step(interp, message);
        self.push_call(
            message.depth,
            Contract {
                caller: message.caller,
                contract: message.destination,
                value: message.value,
                input: message.input.clone(),
            },
            message.kind.into(),
            message.gas_limit,
            interp.gas().refunded(),
        );

        if self.can_call_enter() {
            let call = self.active_call();
            let frame = CallFrame {
                contract: call.contract.clone(),
                kind: FrameKind::Call(call.kind),
                gas: call.gas_limit,
            };
            if let Err(err) = self.try_enter(frame) {
                self.record_hook_error("enter", err);
            }
        }

        self.check_interrupt(interp);
        None
    }

    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if self.check_interrupt(interp) {
            return;
        }
        if self.can_call_exit() {
            let frame_result = FrameResult {
                gas_used: result.gas.spent(),
                output: result.output.clone(),
                error: utils::fmt_error_msg(result.stop, TraceStyle::Geth),
            };
            if let Err(err) = self.try_exit(frame_result) {
                self.record_hook_error("exit", err);
            }
        }

        self.pop_call();
        self.check_interrupt(interp);
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host<'_>,
    ) {
        if self.is_interrupted() || self.hook_errored() {
            return;
        }
        // This is exempt from the root call constraint, because selfdestruct is treated as a
        // new scope that is entered and immediately exited.
        if self.enter_fn.is_some() {
            let frame = CallFrame {
                contract: Contract {
                    caller: *contract,
                    contract: *target,
                    value: *value,
                    input: Bytes::new(),
                },
                kind: FrameKind::SelfDestruct,
                gas: 0,
            };
            if let Err(err) = self.try_enter(frame) {
                self.record_hook_error("enter", err);
            }
        }

        // exit with empty frame result ref <https://github.com/ethereum/go-ethereum/blob/0004c6b229b787281760b14fb9460ffd9c2496f1/core/vm/instructions.go#L829-L829>
        if !self.is_interrupted() && !self.hook_errored() && self.exit_fn.is_some() {
            let frame = FrameResult { gas_used: 0, output: Bytes::new(), error: None };
            if let Err(err) = self.try_exit(frame) {
                self.record_hook_error("exit", err);
            }
        }
    }
}

/// A shared handle for cooperatively interrupting a [`JsInspector`].
///
/// Clones share the same signal. Interruption is permanent and dropping a handle does not
/// interrupt execution. The signal uses relaxed atomics and does not synchronize other data.
/// Use [`Self::drop_guard`] to interrupt execution when the caller is dropped.
#[derive(Clone, Debug, Default)]
pub struct JsInspectorInterrupt(Arc<AtomicBool>);

impl JsInspectorInterrupt {
    /// Creates a handle that has not been interrupted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Signals interruption to all inspectors sharing this handle.
    #[inline]
    pub fn interrupt(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Returns whether interruption has been requested.
    #[inline]
    pub fn is_interrupted(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Creates an owned guard that interrupts execution when dropped.
    ///
    /// The guard shares this handle's signal. Keep it in the request future while the inspector
    /// runs on a blocking worker, so dropping the request also interrupts tracing. Create the
    /// guard before moving it into the future to cover cancellation before the first poll.
    ///
    /// ```
    /// use evm2_inspectors::tracing::js::JsInspectorInterrupt;
    ///
    /// let interrupt = JsInspectorInterrupt::new();
    /// let guard = interrupt.drop_guard();
    /// assert!(!interrupt.is_interrupted());
    /// drop(guard);
    /// assert!(interrupt.is_interrupted());
    /// ```
    pub fn drop_guard(&self) -> JsInspectorInterruptGuard {
        JsInspectorInterruptGuard(self.clone())
    }
}

/// A guard that signals its shared interrupt when dropped.
///
/// Created by [`JsInspectorInterrupt::drop_guard`]. Dropping any guard interrupts all inspectors
/// sharing the signal, even if other handles or guards remain alive.
#[derive(Debug)]
#[must_use = "the guard interrupts tracing immediately if it is not retained"]
pub struct JsInspectorInterruptGuard(JsInspectorInterrupt);

impl Drop for JsInspectorInterruptGuard {
    fn drop(&mut self) {
        self.0.interrupt();
    }
}

/// The evaluated tracer object and its callback functions.
struct JsTracerObject {
    obj: JsObject,
    result_fn: JsObject,
    fault_fn: JsObject,
    enter_fn: Option<JsObject>,
    exit_fn: Option<JsObject>,
    step_fn: Option<JsObject>,
}

impl JsTracerObject {
    /// Evaluates the script to a fresh tracer object, validates its callbacks and invokes `setup`.
    fn evaluate(
        script: &Script,
        config: &serde_json::Value,
        ctx: &mut Context,
    ) -> Result<Self, JsInspectorError> {
        let obj = script.evaluate(ctx).map_err(JsInspectorError::EvalCode)?;
        let obj = obj.as_object().ok_or(JsInspectorError::ExpectedJsObject)?;

        // ensure all the fields are callables, if present

        let result_fn = obj
            .get(js_string!("result"), ctx)?
            .as_object()
            .ok_or(JsInspectorError::ResultFunctionMissing)?;
        if !result_fn.is_callable() {
            return Err(JsInspectorError::ResultFunctionMissing);
        }

        let fault_fn = obj
            .get(js_string!("fault"), ctx)?
            .as_object()
            .ok_or(JsInspectorError::FaultFunctionMissing)?;
        if !fault_fn.is_callable() {
            return Err(JsInspectorError::FaultFunctionMissing);
        }

        let enter_fn = obj.get(js_string!("enter"), ctx)?.as_object().filter(|o| o.is_callable());
        let exit_fn = obj.get(js_string!("exit"), ctx)?.as_object().filter(|o| o.is_callable());
        let step_fn = obj.get(js_string!("step"), ctx)?.as_object().filter(|o| o.is_callable());

        if enter_fn.is_some() != exit_fn.is_some() {
            return Err(JsInspectorError::EnterExitMismatch);
        }

        // Like geth, `setup` receives the config as a JSON string, `{}` if there is none, and a
        // non-callable `setup` is ignored.
        if let Some(setup_fn) = obj.get(js_string!("setup"), ctx)?.as_object()
            && setup_fn.is_callable()
        {
            let config = match config {
                serde_json::Value::Null => js_string!("{}"),
                config => js_string!(config.to_string()),
            };
            setup_fn
                .call(&(obj.clone().into()), &[config.into()], ctx)
                .map_err(JsInspectorError::SetupCallFailed)?;
        }

        Ok(Self { obj, result_fn, fault_fn, enter_fn, exit_fn, step_fn })
    }
}

/// Represents an active call
#[derive(Debug)]
struct CallStackItem {
    /// Unique across calls and transaction resets.
    id: u64,
    contract: Contract,
    kind: CallKind,
    gas_limit: u64,
    /// Child refunds merge into the parent only on success.
    enclosing_refund: i64,
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

    /// The trace object must expose either both or none of `enter()` and `exit()`.
    #[error("trace object must expose either both or none of enter() and exit()")]
    EnterExitMismatch,

    /// Failure during the invocation of the `setup()` function.
    #[error("failed to call setup(): {0}")]
    SetupCallFailed(JsError),

    /// Tracing was cancelled through the shared interrupt flag.
    #[error("JavaScript tracing interrupted")]
    Interrupted,
}

#[derive(Debug, Default)]
struct FaultStep {
    pc: u64,
    op: u8,
    gas: u64,
}

// Geth reports validation failures through step; execution failures also invoke fault.
const fn should_fault(stop: InstrStop, opcode: u8) -> bool {
    match stop {
        InstrStop::StackUnderflow
        | InstrStop::StackOverflow
        | InstrStop::CreateInitCodeSizeLimit
        | InstrStop::CallNotAllowedInsideStatic => false,
        // LOG and TSTORE check write protection during execution, unlike these opcodes.
        InstrStop::StateChangeDuringStaticCall => {
            !matches!(opcode, op::SSTORE | op::CREATE | op::CREATE2 | op::SELFDESTRUCT)
        }
        _ => !stop.is_success() && !stop.is_out_of_gas(),
    }
}

// Preserve native error kinds and source positions, but omit Boa's multiline backtrace.
fn with_message_suffix(err: JsError, suffix: &str, ctx: &mut Context) -> JsError {
    match err.as_engine() {
        Some(EngineError::RuntimeLimit(limit)) => {
            return JsNativeError::range().with_message(format!("{limit}{suffix}")).into();
        }
        Some(engine) => {
            return JsNativeError::error().with_message(format!("{engine}{suffix}")).into();
        }
        None => {}
    }
    let native = if let Some(native) = err.as_native() {
        native.clone()
    } else if let Some(object) = err.as_opaque().and_then(JsValue::as_object)
        && let Some(data) = object.downcast_ref::<Error>()
    {
        // Boa converts aggregate entries and invokes getters; only give it inert own properties.
        let message = core::iter::successors(Some(object.clone()), JsObject::prototype)
            .take(RECURSION_LIMIT)
            .find_map(|object| object.borrow().properties().get(&js_string!("message").into()))
            .and_then(|property| property.value().and_then(JsValue::as_string))
            .map(|message| message.to_std_string_escaped())
            .unwrap_or_default();
        let shallow = JsObject::from_proto_and_data(None, data.clone());
        shallow.insert_property(
            js_string!("message"),
            PropertyDescriptor::builder()
                .value(js_string!(message.as_str()))
                .complete_with_defaults(),
        );
        shallow.insert_property(
            js_string!("errors"),
            PropertyDescriptor::builder()
                .value(JsArray::from_iter([], ctx))
                .complete_with_defaults(),
        );
        shallow.insert_property(
            js_string!("constructor"),
            PropertyDescriptor::builder()
                .value(ctx.intrinsics().constructors().error().constructor())
                .complete_with_defaults(),
        );
        JsError::from_opaque(shallow.into())
            .try_native(ctx)
            .unwrap_or_else(|_| JsNativeError::error())
    } else {
        let thrown = err
            .as_opaque()
            .filter(|value| !value.is_object())
            .map(|value| {
                value
                    .to_string(ctx)
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_else(|_| value.display().to_string())
            })
            .unwrap_or_else(|| "[object Object]".to_string());
        return JsNativeError::error().with_message(format!("{thrown}{suffix}")).into();
    };
    let message = format!("{}{suffix}", native.message());
    native.with_message(message).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec, vec::Vec};
    use alloy_consensus::{TxEip1559, TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, B256, Bytes, TxKind, U256, bytes, hex};
    use boa_engine::error::{EngineError, RuntimeLimitError};
    use evm2::{
        BaseEvmTypes, Evm, Precompiles, SpecId, Version,
        bytecode::Bytecode,
        ethereum::{TxEnvelope, ethereum_tx_registry, intrinsic_and_floor_gas},
        evm::{AccountInfo, CacheDB, EmptyDB},
        interpreter::{Host, op},
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

    #[test]
    fn test_setup_receives_config_json_string() {
        let code = |expected: &str| {
            format!(
                r#"{{
                    setup: function(config) {{
                        if (typeof config !== "string" || config !== '{expected}') {{
                            throw new Error(typeof config + ":" + config);
                        }}
                    }},
                    fault: function() {{}},
                    result: function() {{}}
                }}"#
            )
        };
        JsInspector::new(code(r#"{"a":1}"#), json!({"a": 1})).unwrap();
        JsInspector::new(code("{}"), serde_json::Value::Null).unwrap();
        let result = JsInspector::new(code("{}"), json!({"a": 1}));
        assert!(matches!(result, Err(JsInspectorError::SetupCallFailed(_))));
    }

    #[test]
    fn test_enter_exit_must_come_together() {
        for functions in ["enter: function() {},", "exit: function() {},"] {
            let code = format!("{{{functions} fault: function() {{}}, result: function() {{}}}}");
            let result = JsInspector::new(code, serde_json::Value::Null);
            assert!(matches!(result, Err(JsInspectorError::EnterExitMismatch)));
        }
        let code = "{enter: function() {}, exit: function() {}, fault: function() {}, result: function() {}}";
        JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
    }

    #[test]
    fn test_ctx_matches_geth() {
        let code = r#"{
            fault: function() {},
            result: function(ctx) {
                return [ctx.gasPrice.toString(), ctx.intrinsicGas, ctx.time];
            }
        }"#;
        // The tip is min(max_priority_fee, max_fee - base_fee).
        for (max_fee, expected) in
            [(10_000_000_000u128, "1000000000"), (5_500_000_000, "500000000")]
        {
            let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
            let tx = TxEnvelope::Eip1559(TxEip1559 {
                chain_id: 1,
                gas_limit: 1_000_000,
                max_fee_per_gas: max_fee,
                max_priority_fee_per_gas: 1_000_000_000,
                to: TxKind::Call(Address::repeat_byte(0x01)),
                ..Default::default()
            });
            let res = try_run_tx(insp, None, true, U256::from(5_000_000_000u64), tx).unwrap();
            assert_eq!(res, json!([expected, null, null]));
        }
    }

    #[test]
    fn test_ctx_transaction_fields_need_non_zero_hashes() {
        let code = r#"{
            fault: function() {},
            result: function(ctx) {
                return [ctx.blockHash !== undefined, ctx.txIndex, ctx.txHash !== undefined];
            }
        }"#;
        let hash = Some(B256::repeat_byte(1));
        let zero = Some(B256::ZERO);
        for (block_hash, tx_hash, tx_index, expected) in [
            (None, None, None, json!([false, null, false])),
            (hash, None, Some(3), json!([true, null, false])),
            (zero, hash, Some(3), json!([false, null, false])),
            (hash, zero, Some(3), json!([true, null, false])),
            (hash, hash, Some(3), json!([true, 3, true])),
            (hash, hash, None, json!([true, 0, true])),
        ] {
            let tx_context = TransactionContext { block_hash, tx_index, tx_hash };
            let insp = JsInspector::with_transaction_context(
                code.to_string(),
                serde_json::Value::Null,
                tx_context,
            )
            .unwrap();
            let res = try_run_tx(insp, None, true, U256::ZERO, legacy_tx(1024)).unwrap();
            assert_eq!(res, expected);
        }
    }

    #[test]
    fn test_is_precompiled_is_false_in_setup() {
        let code = r#"{
            before: false,
            setup: function() { this.before = isPrecompiled("0x01"); },
            fault: function() {},
            result: function() { return this.before; }
        }"#;
        let mut insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
        insp.precompiles.set([Address::with_last_byte(1)]);
        insp.fuse().unwrap();
        assert_eq!(try_run_tx(insp, None, true, U256::ZERO, legacy_tx(0)).unwrap(), json!(false));
    }

    fn run_trace(code: &str, contract: Option<Bytes>, success: bool) -> serde_json::Value {
        try_run_trace(code, contract, success).unwrap()
    }

    fn try_run_trace(
        code: &str,
        contract: Option<Bytes>,
        success: bool,
    ) -> Result<serde_json::Value, JsInspectorError> {
        let insp = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
        try_run_tx(insp, contract, success, U256::ZERO, legacy_tx(1024))
    }

    fn legacy_tx(gas_price: u128) -> TxEnvelope {
        TxEnvelope::Legacy(TxLegacy {
            gas_price,
            gas_limit: 1_000_000,
            to: TxKind::Call(Address::repeat_byte(0x01)),
            ..Default::default()
        })
    }

    fn try_run_tx(
        insp: JsInspector,
        contract: Option<Bytes>,
        success: bool,
        basefee: U256,
        tx: TxEnvelope,
    ) -> Result<serde_json::Value, JsInspectorError> {
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

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            evm2::env::BlockEnvExt { basefee, ..Default::default() },
            ethereum_tx_registry(SpecId::CANCUN),
            db,
            Precompiles::base(SpecId::CANCUN),
        );
        evm.set_inspector(insp);

        let tx = Recovered::new_unchecked(tx, Address::ZERO);
        let res = evm.transact(&tx).expect("pass without error").detach();

        assert_eq!(res.result.status, success);
        let mut inspector = evm.clear_inspector_as::<JsInspector>().unwrap();
        let block = *evm.block_env();
        inspector.json_result(&res, &tx, &block, evm.database_mut())
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
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_memory_slice_rejects_non_finite_indexes() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.memory.slice(Infinity, NaN)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_memory_slice_rejects_non_finite_end() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.memory.slice(0, Infinity)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
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
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_stack_peek() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.stack.peek(-1)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_stack_peek_nan() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.stack.peek(NaN)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_stack_peek_infinity() {
        let code = r#"{
            depths: [],
            step: function(log) { this.depths.push(log.stack.peek(Infinity)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_memory_get_uint() {
        let code = r#"{
            depths: [],
            step: function(log, db) { this.depths.push(log.memory.getUint(-64)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_memory_get_uint_rejects_non_finite_offset() {
        let code = r#"{
            depths: [],
            step: function(log, db) { this.depths.push(log.memory.getUint(Infinity)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_memory_get_uint_rejects_nan_offset() {
        let code = r#"{
            depths: [],
            step: function(log, db) { this.depths.push(log.memory.getUint(NaN)); },
            fault: function() {},
            result: function() { return this.depths; }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
    }

    #[test]
    fn test_db_get_state_reads_current_storage() {
        let code = r#"{
            res: [],
            read: function(address, db) {
                return Number(BigInt(toHex(db.getState(address, toWord('0x00')))));
            },
            step: function(log, db) {
                var op = log.op.toString();
                if (op === 'SSTORE' || op === 'STOP') { this.res.push([op, this.read(log.contract.getAddress(), db)]); }
            },
            fault: function() {},
            result: function(ctx, db) {
                this.res.push(['result', this.read(ctx.to, db)]);
                return this.res;
            }
        }"#;
        // SSTORE(0, 1), SSTORE(0, 2), STOP. The SSTORE steps run after their opcode, so they read
        // their own write, where geth reads the value before it.
        let res = run_trace(code, Some(bytes!("0x60016000556002600055")), true);
        assert_eq!(res, json!([["SSTORE", 1], ["SSTORE", 2], ["STOP", 2], ["result", 2]]));
    }

    #[test]
    fn test_db_get_state_of_created_accounts_in_result() {
        let code = r#"{
            enter: function(frame) { this.created = frame.getTo(); },
            exit: function() {},
            fault: function() {},
            result: function(ctx, db) {
                return [toHex(db.getState(this.created, toWord('0x00'))), toHex(db.getState(this.created, toWord('0x01')))];
            }
        }"#;
        // CREATE of an initcode that runs SSTORE(0, 7), then either stops or self-destructs.
        let slot = "0x0000000000000000000000000000000000000000000000000000000000000007";
        let zero = "0x0000000000000000000000000000000000000000000000000000000000000000";
        for (initcode, expected) in [
            ("6007600055", json!([slot, zero])),
            ("6007600055730101010101010101010101010101010101010101ff", json!([zero, zero])),
        ] {
            let initcode = hex::decode(initcode).unwrap();
            // PUSH<n> initcode, PUSH0, MSTORE, then CREATE(0, 32 - n, n).
            let n = initcode.len();
            let mut contract = vec![0x5f + n as u8];
            contract.extend_from_slice(&initcode);
            contract.extend_from_slice(&[
                0x5f,
                0x52,
                0x60,
                n as u8,
                0x60,
                32 - n as u8,
                0x5f,
                0xf0,
                0x00,
            ]);
            let res = run_trace(code, Some(contract.into()), true);
            assert_eq!(res, expected);
        }
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
        let res = run_trace(code, Some(contract.into()), true);
        assert_eq!(res, json!([{ "0": 0, "1": 0 }, { "0": 255, "1": 0 }]));
    }

    #[test]
    fn test_memory_limit() {
        let code = r#"{
            res: [],
            step: function(log) { if (log.op.toString() === 'STOP') { this.res.push(log.memory.slice(5, 1025 * 1024)) } },
            fault: function() {},
            result: function() { return this.res }
        }"#;
        let err = try_run_trace(code, None, true).unwrap_err();
        assert!(matches!(err, JsInspectorError::JsError(_)));
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
    fn test_step_sees_pre_execution_stack() {
        let code = r#"{
            res: [],
            step: function(log) {
                if (log.op.toString() === 'ADD') {
                    this.res.push(log.stack.length());
                    this.res.push(log.stack.peek(0));
                    this.res.push(log.stack.peek(1));
                }
                if (log.op.toString() === 'STOP') {
                    this.res.push(log.stack.length());
                    this.res.push(log.stack.peek(0));
                }
            },
            fault: function() {},
            result: function() { return this.res }
        }"#;
        // PUSH1 1, PUSH1 2, ADD, STOP
        let res = run_trace(code, Some(bytes!("0x600160020100")), true);
        assert_eq!(res, json!([2, "2", "1", 1, "3"]));
    }

    #[test]
    fn test_step_sees_pre_execution_memory() {
        let code = r#"{
            res: [],
            step: function(log) {
                var op = log.op.toString();
                if (op === 'MSTORE8' || op === 'STOP') {
                    this.res.push(log.memory.length());
                    if (log.memory.length() > 0) {
                        this.res.push(log.memory.getUint(0)[0]);
                        this.res.push(log.memory.slice(0, 2)[0]);
                    }
                }
            },
            fault: function() {},
            result: function() { return this.res }
        }"#;
        // PUSH1 0xff, PUSH1 0, MSTORE8, PUSH1 0xaa, PUSH1 0, MSTORE8, STOP
        let res = run_trace(code, Some(bytes!("0x60ff60005360aa60005300")), true);
        // the second MSTORE8 must still see the value written by the first one
        assert_eq!(res, json!([0, 32, 255, 255, 32, 170, 170]));
    }

    #[test]
    fn test_fuse_resets_tracer() {
        let code = r#"{
            count: 0,
            step: function() { this.count += 1; },
            fault: function() {},
            result: function() { return this.count; }
        }"#;
        let addr = Address::repeat_byte(0x01);
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            &Address::ZERO,
            AccountInfo { balance: U256::from(1e18), ..Default::default() },
        );
        db.insert_account_info(
            &addr,
            AccountInfo {
                code: Some(Bytecode::new_legacy(hex!("6001600100").into())),
                ..Default::default()
            },
        );

        let mut inspector = JsInspector::new(code.to_string(), serde_json::Value::Null).unwrap();
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            evm2::env::BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            db,
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 1_000_000,
                to: TxKind::Call(addr),
                ..Default::default()
            }),
            Address::ZERO,
        );
        for _ in 0..3 {
            evm.set_inspector(inspector);
            let res = evm.transact(&tx).unwrap().detach();
            assert!(res.result.status);
            inspector = *evm.clear_inspector_as::<JsInspector>().unwrap();
            let block = *evm.block_env();
            assert_eq!(
                inspector.json_result(&res, &tx, &block, evm.database_mut()).unwrap(),
                json!(3)
            );
            inspector.fuse().unwrap();
        }
    }

    #[test]
    fn test_fuse_resets_callback_objects() {
        let code = r#"{
            counts: { log: 0, db: 0, enter: 0, exit: 0 },
            setup: function(config) {
                this.ready = true;
            },
            mark: function(object, kind) {
                if (!this.ready) throw new Error("setup was not called");
                if (!object.seen) {
                    // Non-configurable properties cannot be removed by clearing the wrapper.
                    Object.defineProperty(object, "seen", { value: true });
                    this.counts[kind]++;
                }
            },
            step: function(log, db) {
                this.mark(log, "log");
                this.mark(db, "db");
            },
            enter: function(frame) { this.mark(frame, "enter"); },
            exit: function(result) { this.mark(result, "exit"); },
            fault: function() {},
            result: function() { return this.counts; }
        }"#;
        let addr = Address::repeat_byte(0x01);
        let child = Address::repeat_byte(0x02);
        // Call the child twice to also verify that wrappers are reused within a transaction.
        let mut bytecode = Vec::new();
        for _ in 0..2 {
            bytecode.extend_from_slice(&hex!("60006000600060006000"));
            bytecode.push(0x73); // PUSH20
            bytecode.extend_from_slice(child.as_slice());
            bytecode.extend_from_slice(&hex!("61fffff150")); // PUSH2 gas, CALL, POP
        }
        bytecode.push(0x00);
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            &addr,
            AccountInfo { code: Some(Bytecode::new_legacy(bytecode.into())), ..Default::default() },
        );
        db.insert_account_info(
            &child,
            AccountInfo {
                code: Some(Bytecode::new_legacy(hex!("600100").into())),
                ..Default::default()
            },
        );
        let mut inspector = JsInspector::new(code.to_string(), json!({})).unwrap();
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            evm2::env::BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            db,
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 1_000_000,
                to: TxKind::Call(addr),
                ..Default::default()
            }),
            Address::ZERO,
        );
        for _ in 0..3 {
            evm.set_inspector(inspector);
            let res = evm.transact(&tx).unwrap().detach();
            assert!(res.result.status);
            inspector = *evm.clear_inspector_as::<JsInspector>().unwrap();
            let block = *evm.block_env();
            assert_eq!(
                inspector.json_result(&res, &tx, &block, evm.database_mut()).unwrap(),
                json!({"log": 1, "db": 1, "enter": 1, "exit": 1})
            );
            inspector.fuse().unwrap();
        }
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

    #[test]
    fn test_runtime_limits() {
        let inspector = JsInspector::new(
            "{fault: function() {}, result: function() {}}".into(),
            serde_json::Value::Null,
        )
        .unwrap();
        assert_eq!(inspector.ctx.runtime_limits().recursion_limit(), RECURSION_LIMIT);
        assert_eq!(inspector.ctx.runtime_limits().loop_iteration_limit(), LOOP_ITERATION_LIMIT);
    }

    #[test]
    fn test_accessor_recursion_limit() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                for setup in [
                    "const obj = {get x() {return this.x;}}; obj.x;",
                    "const obj = {set x(value) {this.x = value;}}; obj.x = 1;",
                    "async function* f() {} f().return({get then() {this.then;}});",
                ] {
                    let code = format!(
                        "{{setup: function() {{{setup}}}, fault: function() {{}}, result: function() {{}}}}"
                    );
                    let error = JsInspector::new(code, serde_json::Value::Null).unwrap_err();
                    let JsInspectorError::SetupCallFailed(error) = error else {
                        panic!("unexpected error: {error}");
                    };
                    assert!(matches!(
                        error.as_engine(),
                        Some(EngineError::RuntimeLimit(RuntimeLimitError::Recursion))
                    ));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_create_context_to_on_success_revert_halt_and_runtime_oog() {
        for (spec, initcode, runtime_oog) in [
            (SpecId::PRAGUE, Bytes::from_static(&[op::STOP]), false),
            (SpecId::PRAGUE, Bytes::from_static(&[op::PUSH0, op::PUSH0, op::REVERT]), false),
            (SpecId::PRAGUE, Bytes::from_static(&[op::INVALID]), false),
            (SpecId::AMSTERDAM, Bytes::from_static(&[op::STOP]), true),
        ] {
            let caller = Address::repeat_byte(0xaa);
            let nonce = 7;
            let mut db = CacheDB::new(EmptyDB::default());
            db.insert_account_info(&caller, AccountInfo { nonce, ..AccountInfo::default() });
            let script = r#"{
                fault: function() {},
                result: function(ctx) { return toHex(ctx.to); }
            }"#;
            let inspector = JsInspector::new(script.to_string(), serde_json::Value::Null).unwrap();
            let mut evm = Evm::<BaseEvmTypes>::new(
                spec,
                evm2::env::BlockEnvExt::default(),
                ethereum_tx_registry(spec),
                db,
                Precompiles::base(spec),
            );
            evm.set_inspector(inspector);
            let gas_limit = if runtime_oog {
                let version = Version::base(spec);
                let (intrinsic, floor) = intrinsic_and_floor_gas(
                    version,
                    caller,
                    TxKind::Create,
                    &initcode,
                    0,
                    0,
                    U256::ZERO,
                );
                intrinsic.max(floor)
            } else {
                100_000
            };
            let tx = Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    nonce,
                    gas_limit,
                    to: TxKind::Create,
                    input: initcode,
                    ..TxLegacy::default()
                }),
                caller,
            );
            let result = evm.transact(&tx).unwrap().detach();
            if runtime_oog {
                assert_eq!(result.result.stop, InstrStop::OutOfGas);
            }
            let mut inspector = evm.clear_inspector_as::<JsInspector>().unwrap();
            let block = *evm.block_env();
            let traced = inspector.json_result(&result, &tx, &block, evm.database_mut()).unwrap();
            assert_eq!(traced, json!(format!("{:#x}", caller.create(nonce))));
        }
    }

    #[test]
    fn memory_slice_padding_boundaries() {
        let tracer = r#"{r:null,step:function(log){if(log.op.toString()!=='STOP')return;
            var a=log.memory.slice(0,33);var capped=log.memory.slice(32,32+1024*1024);var error=null;
            try{log.memory.slice(32,33+1024*1024)}catch(e){error=e.name}
            this.r=[a[0],a[32],capped.length,log.memory.slice(2**40,2**40).length,error];
        },fault:function(){},result:function(){return this.r}}"#;
        assert_eq!(
            run_trace(tracer, Some(hex!("60ff60005300").into()), true),
            json!([255, 0, 1024 * 1024, 0, "TypeError"])
        );
    }

    #[test]
    fn hook_error_normalization_avoids_user_code() {
        for (script, expected) in [
            (
                "const e = new AggregateError([], 'boom'); for (const p of ['cause','errors','constructor']) Object.defineProperty(e,p,{get(){reads++;throw 0}}); throw e;",
                "AggregateError: boom suffix",
            ),
            (
                "const e = new Error(); Object.defineProperty(e,'message',{get(){reads++;throw 0}}); throw e;",
                "Error:  suffix",
            ),
            ("throw new Proxy({}, {get(){reads++;throw 0}})", "Error: [object Object] suffix"),
            (
                "const e = new Error(); e.message = {toString(){reads++;throw 0}}; throw e;",
                "Error:  suffix",
            ),
            (
                "class E extends TypeError {}; E.prototype.message = 'inherited'; throw new E();",
                "TypeError: inherited suffix",
            ),
            (
                "const e = new Error(); let p = {message:'hidden'}; for (let i=0;i<1024;i++) p=Object.create(p); Object.setPrototypeOf(e,p); throw e;",
                "Error:  suffix",
            ),
        ] {
            let mut ctx = Context::default();
            let script = format!("globalThis.reads = 0; {script}");
            let err = ctx.eval(Source::from_bytes(&script)).unwrap_err();
            let error = with_message_suffix(err, " suffix", &mut ctx);
            assert_eq!(ctx.eval(Source::from_bytes("reads")).unwrap(), JsValue::from(0));
            let native = error.as_native().unwrap();
            assert_eq!(format!("{}: {}", native.kind(), native.message()), expected);
        }
    }

    #[test]
    fn hook_error_preserves_kind_and_location() {
        let mut ctx = Context::default();
        let err = ctx.eval(Source::from_bytes("throw new TypeError('boom')")).unwrap_err();
        let expected = err.try_native(&mut ctx).unwrap().with_message("boom suffix");
        let actual = with_message_suffix(err, " suffix", &mut ctx);
        assert_eq!(actual.as_native().unwrap(), &expected);
    }
}
