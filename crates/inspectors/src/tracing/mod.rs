//! EVM tracing inspectors.

use crate::{
    opcode::immediate_size,
    tracing::{
        arena::PushTraceKind,
        types::{
            CallKind, CallLog, CallTrace, CallTraceStep, RecordedMemory, StorageChange,
            StorageChangeReason, TraceMemberOrder,
        },
        utils::gas_used,
    },
};
use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use evm2::{
    Evm, EvmTypes, Inspector, SpecId,
    bytecode::opcode::{OpCode, op},
    evm::StateChanges,
    interpreter::{Interpreter, Message, MessageKind, MessageResult},
};

mod arena;
pub use arena::CallTraceArena;

mod builder;
pub use builder::{
    geth::{self, GethTraceBuilder},
    parity::{self, ParityTraceBuilder},
};

mod config;
pub use config::{OpcodeFilter, StackSnapshotType, TracingInspectorConfig};

mod fourbyte;
pub use fourbyte::FourByteInspector;

mod opcount;
pub use opcount::OpcodeCountInspector;

pub mod types;

mod utils;

#[cfg(feature = "std")]
mod writer;
#[cfg(feature = "std")]
pub use writer::{TraceWriter, TraceWriterConfig};

/// JavaScript tracing support.
#[cfg(feature = "js-tracer")]
#[allow(dead_code)]
pub mod js {
    use crate::tracing::{
        TransactionContext,
        js::{
            bindings::{
                CallFrame, Contract, EvmDbRef, FrameResult, JsEvmContext, MemoryRef, OpObj,
                StackRef, StepLog,
            },
            builtins::{PrecompileList, to_serde_value},
        },
        types::CallKind,
    };
    use alloc::string::String;
    use alloy_primitives::{Address, Bytes, TxKind, U256, map::HashSet};
    use boa_engine::{Context, JsError, JsObject, JsValue, Source, js_string};
    use evm2::{
        Evm, EvmTypes, Inspector,
        evm::{CacheDB, EmptyDB},
        interpreter::{GasTracker, InstrStop, Interpreter, Message, MessageResult},
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
        step_error: bool,
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
        /// - `step`: a function that will be called when the execution steps to the next
        ///   instruction.
        pub fn new(code: String, config: serde_json::Value) -> Result<Self, JsInspectorError> {
            let mut ctx = Context::default();

            ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
            ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);

            register_builtins(&mut ctx)?;

            let wrapped = alloc::format!("({code})");
            let obj = ctx
                .eval(Source::from_bytes(wrapped.as_bytes()))
                .map_err(JsInspectorError::EvalCode)?;

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

            let _js_config_value = JsValue::from_json(&config, &mut ctx)
                .map_err(JsInspectorError::InvalidJsonConfig)?;

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
                transaction_context: TransactionContext::default(),
                precompiles_registered: false,
                step_error: false,
            })
        }

        /// Set contextual transaction info.
        pub const fn set_transaction_context(&mut self, transaction_context: TransactionContext) {
            self.transaction_context = transaction_context;
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
            let mut cloned = Self::new(self.code.clone(), self.config.clone())?;
            cloned.set_transaction_context(self.transaction_context);
            Ok(cloned)
        }

        /// Calls the result function with an evm2-native trace context.
        pub fn json_result_from_parts(
            &mut self,
            result: JsTraceResult,
            tx: JsTraceTx,
            block: JsTraceBlock,
            db: &CacheDB<EmptyDB>,
        ) -> Result<serde_json::Value, JsInspectorError> {
            let result = self.result_from_parts(result, tx, block, db)?;
            Ok(to_serde_value(result, &mut self.ctx)?)
        }

        /// Calls the result function with an evm2-native trace context.
        pub fn result_from_parts(
            &mut self,
            result: JsTraceResult,
            tx: JsTraceTx,
            block: JsTraceBlock,
            db: &CacheDB<EmptyDB>,
        ) -> Result<JsValue, JsInspectorError> {
            let mut to = None;
            let mut error = None;
            if result.success {
                if let TxKind::Call(target) = tx.kind {
                    to = Some(target);
                } else {
                    to = result.created_address;
                }
            } else if result.stop.is_revert() {
                error = Some("execution reverted".to_string());
            } else {
                error = Some(format!("execution halted: {:?}", result.stop));
            }

            let ctx = JsEvmContext {
                r#type: match tx.kind {
                    TxKind::Call(_) => "CALL",
                    TxKind::Create => "CREATE",
                }
                .to_string(),
                from: tx.caller,
                to,
                input: tx.input,
                gas: tx.gas_limit,
                gas_used: result.gas_used,
                gas_price: tx.gas_price.try_into().unwrap_or(u64::MAX),
                value: tx.value,
                block: block.number,
                coinbase: block.coinbase,
                output: result.output,
                time: block.timestamp.to_string(),
                intrinsic_gas: 0,
                transaction_ctx: self.transaction_context,
                error,
            };
            let ctx = ctx.into_js_object(&mut self.ctx)?;
            let (db, _db_guard) = EvmDbRef::new(db);
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

        fn active_call(&self) -> CallStackItem {
            self.call_stack.last().cloned().unwrap_or_default()
        }

        fn push_call<T: EvmTypes>(&mut self, message: &Message<T>) {
            let call = CallStackItem {
                contract: Contract {
                    caller: message.caller,
                    contract: message.destination,
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

        const fn can_call_enter(&self) -> bool {
            self.enter_fn.is_some()
        }

        const fn can_call_exit(&self) -> bool {
            self.exit_fn.is_some()
        }

        fn try_step(&mut self, step: StepLog, db: EvmDbRef) -> Result<(), JsError> {
            let Some(step_fn) = &self.step_fn else { return Ok(()) };
            let js_step = step.into_js_object(&mut self.ctx)?;
            let db = db.into_js_object(&mut self.ctx)?;
            step_fn.call(
                &(self.obj.clone().into()),
                &[js_step.into(), db.into()],
                &mut self.ctx,
            )?;
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
            let precompiles =
                PrecompileList(HashSet::from_iter(host.precompiles().warm_addresses()));
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

    /// Execution result data passed to JavaScript `result`.
    #[derive(Clone, Debug)]
    pub struct JsTraceResult {
        /// Whether execution succeeded.
        pub success: bool,
        /// Gas used by the transaction.
        pub gas_used: u64,
        /// Interpreter stop reason.
        pub stop: InstrStop,
        /// Return or revert output.
        pub output: Bytes,
        /// Created contract address for create transactions.
        pub created_address: Option<Address>,
    }

    /// Transaction data passed to JavaScript `result`.
    #[derive(Clone, Debug)]
    pub struct JsTraceTx {
        /// Transaction caller.
        pub caller: Address,
        /// Transaction target.
        pub kind: TxKind,
        /// Transaction input.
        pub input: Bytes,
        /// Transaction gas limit.
        pub gas_limit: u64,
        /// Transaction gas price.
        pub gas_price: u128,
        /// Transaction value.
        pub value: U256,
    }

    /// Block data passed to JavaScript `result`.
    #[derive(Clone, Copy, Debug)]
    pub struct JsTraceBlock {
        /// Block number.
        pub number: u64,
        /// Block beneficiary.
        pub coinbase: Address,
        /// Block timestamp.
        pub timestamp: U256,
    }

    impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for JsInspector {
        fn step(&mut self, interp: &mut Interpreter<'_, T>, host: &mut T::Host) {
            self.register_precompiles(host);

            if self.step_fn.is_none() || self.step_error {
                return;
            }

            let empty_db = CacheDB::new(EmptyDB::default());
            let (db, _db_guard) = EvmDbRef::new(&empty_db);
            let (stack, _stack_guard) = StackRef::new(interp.stack());
            let (memory, _memory_guard) = MemoryRef::new(interp.memory_ref());
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
                    input: active_call.contract.input,
                },
            };

            self.set_previous_gas_spent(gas_spent);

            if self.try_step(step, db).is_err() {
                self.step_error = true;
            }
        }

        fn step_end(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
            if self.step_fn.is_none() || self.step_error || interp.result().is_ok() {
                return;
            }

            let empty_db = CacheDB::new(EmptyDB::default());
            let (db, _db_guard) = EvmDbRef::new(&empty_db);
            let (stack, _stack_guard) = StackRef::new(interp.stack());
            let (memory, _memory_guard) = MemoryRef::new(interp.memory_ref());
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
                error: interp.result().err().map(|err| format!("{err:?}")),
                contract: Contract {
                    caller: message.caller,
                    contract: message.destination,
                    value: active_call.contract.value,
                    input: active_call.contract.input,
                },
            };

            let _ = self.try_fault(step, db);
        }

        fn call(
            &mut self,
            message: &mut Message<T>,
            _host: &mut T::Host,
        ) -> Option<MessageResult<T>> {
            self.push_call(message);
            if self.can_call_enter() {
                let call = self.active_call();
                let frame =
                    CallFrame { contract: call.contract, kind: call.kind, gas: call.gas_limit };
                if self.try_enter(frame).is_err() {
                    return Some(js_error_to_revert(message.gas_limit));
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
                    error: (!result.stop.is_success()).then(|| format!("{:?}", result.stop)),
                };
                if self.try_exit(frame_result).is_err() {
                    *result = js_error_to_revert(result.gas.limit());
                }
            }

            if self.step_error && result.stop.is_success() {
                result.stop = InstrStop::Revert;
            }

            self.pop_call();
        }

        fn create(
            &mut self,
            message: &mut Message<T>,
            _host: &mut T::Host,
        ) -> Option<MessageResult<T>> {
            self.push_call(message);
            if self.can_call_enter() {
                let call = self.active_call();
                let frame =
                    CallFrame { contract: call.contract, kind: call.kind, gas: call.gas_limit };
                if self.try_enter(frame).is_err() {
                    return Some(js_error_to_revert(message.gas_limit));
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
                    error: (!result.stop.is_success()).then(|| format!("{:?}", result.stop)),
                };
                if self.try_exit(frame_result).is_err() {
                    *result = js_error_to_revert(result.gas.limit());
                }
            }

            if self.step_error && result.stop.is_success() {
                result.stop = InstrStop::Revert;
            }

            self.pop_call();
        }
    }

    fn js_error_to_revert<T: EvmTypes>(gas_limit: u64) -> MessageResult<T> {
        MessageResult {
            stop: InstrStop::Revert,
            gas: GasTracker::new(gas_limit),
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
        use alloc::{rc::Rc, vec, vec::Vec};
        use alloy_consensus::{TxLegacy, transaction::Recovered};
        use alloy_primitives::{Address, Bytes, TxKind, U256, hex};
        use core::cell::RefCell;
        use evm2::{
            BaseEvmTypes, Evm, Precompiles, SpecId,
            bytecode::Bytecode,
            ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
            evm::AccountInfo,
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
            result: evm2::TxResult,
            target: Address,
            gas_price: u128,
            db: &CacheDB<EmptyDB>,
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
                output: result.output,
                time: "0".to_string(),
                intrinsic_gas: 0,
                transaction_ctx: TransactionContext::default(),
                error,
            };
            let ctx = ctx.into_js_object(&mut insp.ctx).unwrap();
            let (db, _db_guard) = EvmDbRef::new(db);
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
            let db = evm.database_as::<CacheDB<EmptyDB>>().unwrap();
            js_result(&mut insp.borrow_mut(), res, addr, gas_price, db)
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
            assert_eq!(
                res.as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(),
                0
            );
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
            assert_eq!(
                res.as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(),
                0
            );
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
                res.as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap_or(0))
                    .collect::<Vec<u64>>(),
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
}

mod mux;
pub use mux::{Error as MuxError, MuxInspector};

mod debug;
pub use debug::{DebugInspector, DebugInspectorError, DebugTraceResult, TraceBlockEnv, TraceTxEnv};

/// An inspector that collects call traces.
#[derive(Clone, Debug)]
pub struct TracingInspector {
    config: TracingInspectorConfig,
    traces: CallTraceArena,
    trace_stack: Vec<usize>,
    step_stack: Vec<(usize, usize, u64, usize)>,
    log_index: u64,
    spec_id: Option<SpecId>,
}

impl Default for TracingInspector {
    fn default() -> Self {
        Self::new(TracingInspectorConfig::default())
    }
}

impl TracingInspector {
    /// Returns a new instance for the given config.
    pub fn new(config: TracingInspectorConfig) -> Self {
        Self {
            config,
            traces: CallTraceArena::default(),
            trace_stack: Vec::new(),
            step_stack: Vec::new(),
            log_index: 0,
            spec_id: None,
        }
    }

    /// Resets the inspector to its initial state.
    pub fn fuse(&mut self) {
        self.traces.clear();
        self.trace_stack.clear();
        self.step_stack.clear();
        self.log_index = 0;
    }

    /// Resets the inspector to its initial state.
    pub fn fused(mut self) -> Self {
        self.fuse();
        self
    }

    /// Returns the config of the inspector.
    pub const fn config(&self) -> &TracingInspectorConfig {
        &self.config
    }

    /// Returns a mutable reference to the config of the inspector.
    pub const fn config_mut(&mut self) -> &mut TracingInspectorConfig {
        &mut self.config
    }

    /// Updates the config of the inspector.
    pub fn update_config(
        &mut self,
        f: impl FnOnce(TracingInspectorConfig) -> TracingInspectorConfig,
    ) {
        self.config = f(self.config);
    }

    /// Gets a reference to the recorded call traces.
    pub const fn traces(&self) -> &CallTraceArena {
        &self.traces
    }

    /// Gets a mutable reference to the recorded call traces.
    pub const fn traces_mut(&mut self) -> &mut CallTraceArena {
        &mut self.traces
    }

    /// Consumes the inspector and returns the recorded call traces.
    pub fn into_traces(self) -> CallTraceArena {
        self.traces
    }

    /// Sets the root transaction gas used.
    #[inline]
    pub fn set_transaction_gas_used(&mut self, gas_used: u64) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.gas_used = gas_used;
        }
    }

    /// Sets the root transaction gas limit.
    #[inline]
    pub fn set_transaction_gas_limit(&mut self, gas_limit: u64) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.gas_limit = gas_limit;
        }
    }

    /// Sets the root transaction caller.
    #[inline]
    pub fn set_transaction_caller(&mut self, caller: Address) {
        if let Some(node) = self.traces.arena.first_mut() {
            node.trace.caller = caller;
        }
    }

    /// Sets the root transaction gas used and returns the inspector.
    #[inline]
    pub fn with_transaction_gas_used(mut self, gas_used: u64) -> Self {
        self.set_transaction_gas_used(gas_used);
        self
    }

    /// Fills storage changes from transaction state changes and recorded SSTORE stack values.
    pub fn fill_storage_changes(&mut self, state: &StateChanges) {
        let mut current_storage = BTreeMap::new();
        for (&address, storage) in &state.storage {
            for (&key, slot) in &storage.slots {
                current_storage.insert((address, key), slot.original);
            }
        }

        let mut changes = Vec::new();
        self.collect_sstore_changes(0, &mut changes);

        for (node_idx, step_idx, address, key, value) in changes {
            let Some(had_value) = current_storage.get_mut(&(address, key)) else {
                continue;
            };
            let step = &mut self.traces.arena[node_idx].trace.steps[step_idx];
            step.storage_change = Some(Box::new(StorageChange {
                key,
                value,
                had_value: Some(*had_value),
                reason: StorageChangeReason::SSTORE,
            }));
            *had_value = value;
        }
    }

    fn collect_sstore_changes(
        &self,
        node_idx: usize,
        changes: &mut Vec<(usize, usize, Address, U256, U256)>,
    ) {
        let Some(node) = self.traces.arena.get(node_idx) else {
            return;
        };
        let address = node.execution_address();

        for order in &node.ordering {
            match *order {
                TraceMemberOrder::Step(step_idx) => {
                    let Some(step) = node.trace.steps.get(step_idx) else {
                        continue;
                    };
                    if step.op.get() != op::SSTORE || step.storage_change.is_some() {
                        continue;
                    }
                    let Some(stack) = &step.stack else {
                        continue;
                    };
                    let Some((key, value)) = stack
                        .split_last()
                        .and_then(|(&key, stack)| stack.last().map(|value| (key, *value)))
                    else {
                        continue;
                    };
                    changes.push((node_idx, step_idx, address, key, value));
                }
                TraceMemberOrder::Call(child_idx) => {
                    let Some(child_idx) = node.children.get(child_idx).copied() else {
                        continue;
                    };
                    self.collect_sstore_changes(child_idx, changes);
                }
                TraceMemberOrder::Log(_) => {}
            }
        }
    }

    /// Returns a geth trace builder over the recorded traces.
    #[inline]
    pub fn geth_builder(&self) -> GethTraceBuilder<'_> {
        GethTraceBuilder::new_borrowed(self.traces.nodes(), self.spec_id)
    }

    /// Consumes the inspector and returns a geth trace builder.
    #[inline]
    pub fn into_geth_builder(self) -> GethTraceBuilder<'static> {
        GethTraceBuilder::new(self.traces.into_nodes(), self.spec_id)
    }

    /// Consumes the inspector and returns a parity trace builder.
    #[inline]
    pub fn into_parity_builder(self) -> ParityTraceBuilder {
        ParityTraceBuilder::new(self.traces.into_nodes(), self.spec_id, self.config)
    }

    const fn is_deep(&self) -> bool {
        !self.trace_stack.is_empty()
    }

    fn is_precompile_call<T: EvmTypes<Host = Evm<T>>>(
        &self,
        host: &Evm<T>,
        message: &Message<T>,
    ) -> bool {
        !message.disable_precompiles
            && self.is_deep()
            && message.value.is_zero()
            && host.precompiles().contains(&message.code_address)
    }

    fn start_trace<T: EvmTypes>(&mut self, message: &Message<T>, maybe_precompile: Option<bool>) {
        let caller = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => message.destination,
            _ => message.caller,
        };
        let address = match message.kind {
            MessageKind::DelegateCall | MessageKind::CallCode => message.code_address,
            _ => message.destination,
        };
        let trace = CallTrace {
            depth: usize::from(message.depth),
            caller,
            address,
            maybe_precompile,
            kind: message.kind.into(),
            value: message.value,
            data: message.input.clone(),
            gas_limit: message.gas_limit,
            ..Default::default()
        };

        let entry = self.trace_stack.last().copied().unwrap_or_default();
        let push_kind = if maybe_precompile.unwrap_or(false) {
            PushTraceKind::PushOnly
        } else {
            PushTraceKind::PushAndAttachToParent
        };
        let idx = self.traces.push_trace(entry, push_kind, trace);
        self.trace_stack.push(idx);
    }

    fn end_trace<T: EvmTypes>(&mut self, result: &MessageResult<T>) {
        let Some(idx) = self.trace_stack.pop() else {
            return;
        };
        let trace = &mut self.traces.arena[idx].trace;
        trace.status = Some(result.stop);
        trace.success = result.stop.is_success();
        trace.output = result.output.clone();
        trace.gas_used = result.gas.spent();
        trace.gas_refund_counter = result.gas.refunded().max(0) as u64;
        if let Some(address) = result.created_address {
            trace.address = address;
        }
    }
}

impl From<MessageKind> for CallKind {
    fn from(kind: MessageKind) -> Self {
        match kind {
            MessageKind::Call => Self::Call,
            MessageKind::StaticCall => Self::StaticCall,
            MessageKind::DelegateCall => Self::DelegateCall,
            MessageKind::CallCode => Self::CallCode,
            MessageKind::Create => Self::Create,
            MessageKind::Create2 => Self::Create2,
            _ => Self::Call,
        }
    }
}

impl<T: EvmTypes<Host = Evm<T>>> Inspector<T> for TracingInspector {
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        self.spec_id = Some(interp.spec());
        if self.trace_stack.is_empty() {
            self.start_trace(interp.message(), None);
        }
    }

    fn step(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        self.spec_id = Some(interp.spec());
        if !self.config.record_steps {
            return;
        }
        let Some(trace_idx) = self.trace_stack.last().copied() else {
            return;
        };
        let op = OpCode::new_or_unknown(interp.opcode());
        if !self.config.should_record_opcode(op) {
            return;
        }

        let stack = if self.config.record_stack_snapshots.is_full()
            || self.config.record_stack_snapshots.is_all()
        {
            Some(Box::from(interp.stack().as_slice()))
        } else {
            None
        };
        let memory = self
            .config
            .record_memory_snapshots
            .then(|| RecordedMemory::new(interp.memory_ref().slice(0, interp.memory_ref().len())));
        let returndata = if self.config.record_returndata_snapshots {
            interp.return_data().clone()
        } else {
            Bytes::new()
        };
        let immediate_bytes = if self.config.record_immediate_bytes {
            let immediate_size = usize::from(immediate_size(op.get()));
            (immediate_size > 0).then(|| {
                let pc = interp.pc() + 1;
                let bytecode = interp.bytecode();
                let bytes = bytecode.as_slice().get(pc..pc + immediate_size).unwrap_or_default();
                Bytes::copy_from_slice(bytes)
            })
        } else {
            None
        };
        let step = CallTraceStep {
            pc: interp.pc(),
            op,
            stack,
            push_stack: None,
            memory,
            returndata,
            gas_remaining: interp.gas().remaining(),
            gas_refund_counter: interp.gas().refunded().max(0) as u64,
            gas_used: gas_used(
                interp.spec(),
                interp.gas().spent(),
                interp.gas().refunded().max(0) as u64,
            ),
            gas_cost: 0,
            storage_change: None,
            status: None,
            immediate_bytes,
            decoded: None,
        };
        let step_idx = self.traces.arena[trace_idx].trace.steps.len();
        self.traces.arena[trace_idx].ordering.push(TraceMemberOrder::Step(step_idx));
        self.traces.arena[trace_idx].trace.steps.push(step);
        self.step_stack.push((trace_idx, step_idx, interp.gas().remaining(), interp.stack().len()));
    }

    fn step_end(&mut self, interp: &mut Interpreter<'_, T>, _host: &mut T::Host) {
        if !self.config.record_steps {
            return;
        }
        let Some((trace_idx, step_idx, gas_remaining, stack_len_before)) = self.step_stack.pop()
        else {
            return;
        };
        if let Some(step) = self.traces.arena[trace_idx].trace.steps.get_mut(step_idx) {
            step.gas_cost = gas_remaining.saturating_sub(interp.gas().remaining());
            step.status = interp.result().err();
            if self.config.record_stack_snapshots.is_pushes()
                || self.config.record_stack_snapshots.is_all()
            {
                let stack = interp.stack();
                if stack.len() > stack_len_before {
                    step.push_stack = Some(Box::from(&stack.as_slice()[stack_len_before..]));
                }
            }
        }
    }

    fn log(&mut self, log: &Log, _host: &mut T::Host) {
        if !self.config.record_logs {
            return;
        }
        if let Some(trace_idx) = self.trace_stack.last().copied() {
            let node = &mut self.traces.arena[trace_idx];
            let log_idx = node.log_count();
            node.ordering.push(TraceMemberOrder::Log(log_idx));
            node.logs.push(
                CallLog::from(log.clone()).with_position(log_idx as u64).with_index(self.log_index),
            );
            self.log_index += 1;
        }
    }

    fn call(&mut self, message: &mut Message<T>, host: &mut T::Host) -> Option<MessageResult<T>> {
        let maybe_precompile =
            self.config.exclude_precompile_calls.then(|| self.is_precompile_call(host, message));
        self.start_trace(message, maybe_precompile);
        None
    }

    fn call_end(
        &mut self,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
        _host: &mut T::Host,
    ) {
        self.end_trace(result);
    }

    fn create(
        &mut self,
        message: &mut Message<T>,
        _host: &mut T::Host,
    ) -> Option<MessageResult<T>> {
        self.start_trace(message, Some(false));
        None
    }

    fn create_end(
        &mut self,
        _message: &Message<T>,
        result: &mut MessageResult<T>,
        _host: &mut T::Host,
    ) {
        self.end_trace(result);
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut T::Host,
    ) {
        if let Some(trace_idx) = self.trace_stack.last().copied() {
            let trace = &mut self.traces.arena[trace_idx].trace;
            trace.selfdestruct_address = Some(*contract);
            trace.selfdestruct_refund_target = Some(*target);
            trace.selfdestruct_transferred_value = Some(*value);
        }
    }
}

/// Contextual transaction info made available to debug tracers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionContext {
    /// Hash of the block the transaction is contained within.
    pub block_hash: Option<B256>,
    /// Index of the transaction within a block.
    pub tx_index: Option<usize>,
    /// Hash of the transaction being traced.
    pub tx_hash: Option<B256>,
}

impl TransactionContext {
    /// Sets the block hash.
    pub const fn with_block_hash(mut self, block_hash: B256) -> Self {
        self.block_hash = Some(block_hash);
        self
    }

    /// Sets the index of the transaction within a block.
    pub const fn with_tx_index(mut self, tx_index: usize) -> Self {
        self.tx_index = Some(tx_index);
        self
    }

    /// Sets the transaction hash.
    pub const fn with_tx_hash(mut self, tx_hash: B256) -> Self {
        self.tx_hash = Some(tx_hash);
        self
    }
}

impl From<alloy_rpc_types_eth::TransactionInfo> for TransactionContext {
    fn from(tx_info: alloy_rpc_types_eth::TransactionInfo) -> Self {
        Self {
            block_hash: tx_info.block_hash,
            tx_index: tx_info.index.map(|idx| idx as usize),
            tx_hash: tx_info.hash,
        }
    }
}
