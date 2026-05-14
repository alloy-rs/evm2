//! Type bindings for js tracing inspector

use crate::tracing::{
    TransactionContext,
    js::builtins::{
        address_to_uint8_array, address_to_uint8_array_value, bytes_from_value, bytes_to_address,
        bytes_to_b256, to_bigint, to_uint8_array, to_uint8_array_value,
    },
    types::CallKind,
};
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, U256};
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsValue, js_string,
    native_function::NativeFunction,
    object::{FunctionObjectBuilder, builtins::JsUint8Array},
};
use boa_gc::{Finalize, Trace, empty_trace};
use core::cell::RefCell;
use evm2::{
    bytecode::opcode::{OpCode, op},
    evm::{AccountInfo, CacheDB, EmptyDB},
    interpreter::{Memory, StackRef as EvmStackRef, Word},
};

macro_rules! js_value_getter {
    ($value:ident, $ctx:ident) => {
        FunctionObjectBuilder::new(
            $ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| Ok(JsValue::from($value))),
        )
        .length(0)
        .build()
    };
}

macro_rules! js_value_capture_getter {
    ($value:ident, $ctx:ident) => {
        FunctionObjectBuilder::new(
            $ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, _args, input, _ctx| Ok(JsValue::from(input.clone())),
                $value,
            ),
        )
        .length(0)
        .build()
    };
}

#[derive(Clone, Debug)]
struct GuardedNullableGc<Val: 'static> {
    inner: Rc<RefCell<Option<Guarded<'static, Val>>>>,
}

impl<Val: 'static> GuardedNullableGc<Val> {
    fn new_ref(val: &Val) -> (Self, GcGuard<'_, Val>) {
        Self::new(Guarded::Ref(val))
    }

    fn new_owned<'a>(val: Val) -> (Self, GcGuard<'a, Val>) {
        Self::new(Guarded::Owned(val))
    }

    fn new(val: Guarded<'_, Val>) -> (Self, GcGuard<'_, Val>) {
        let inner = Rc::new(RefCell::new(Some(val)));
        let guard = GcGuard { inner: Rc::clone(&inner) };

        // SAFETY: guard enforces that the value is removed from the refcell before it is dropped.
        #[allow(clippy::missing_transmute_annotations)]
        let this = Self { inner: unsafe { core::mem::transmute(inner) } };

        (this, guard)
    }

    fn with_inner<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&Val) -> R,
    {
        self.inner.borrow().as_ref().map(|guard| f(guard.as_ref()))
    }
}

impl<Val: 'static> Finalize for GuardedNullableGc<Val> {}

unsafe impl<Val: 'static> Trace for GuardedNullableGc<Val> {
    empty_trace!();
}

#[derive(Debug)]
enum Guarded<'a, T> {
    Ref(&'a T),
    Owned(T),
}

impl<T> Guarded<'_, T> {
    #[inline]
    const fn as_ref(&self) -> &T {
        match self {
            Self::Ref(val) => val,
            Self::Owned(val) => val,
        }
    }
}

#[derive(Debug)]
#[must_use]
pub(crate) struct GcGuard<'a, Val> {
    inner: Rc<RefCell<Option<Guarded<'a, Val>>>>,
}

impl<Val> Drop for GcGuard<'_, Val> {
    fn drop(&mut self) {
        self.inner.borrow_mut().take();
    }
}

/// The Log object that is passed to the javascript inspector.
#[derive(Debug)]
pub(crate) struct StepLog {
    /// Stack before step execution
    pub(crate) stack: StackRef,
    /// Opcode to be executed
    pub(crate) op: OpObj,
    /// All allocated memory in a step
    pub(crate) memory: MemoryRef,
    /// Program counter before step execution
    pub(crate) pc: u64,
    /// Remaining gas before step execution
    pub(crate) gas_remaining: u64,
    /// Gas cost of step execution
    pub(crate) cost: u64,
    /// Call depth
    pub(crate) depth: u64,
    /// Gas refund counter before step execution
    pub(crate) refund: u64,
    /// returns information about the error if one occurred, otherwise returns undefined
    pub(crate) error: Option<String>,
    /// The contract object available to the js inspector
    pub(crate) contract: Contract,
}

impl StepLog {
    /// Converts the contract object into a js object
    ///
    /// Caution: this expects a global property `bigint` to be present.
    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let Self {
            stack,
            op,
            memory,
            pc,
            gas_remaining: gas,
            cost,
            depth,
            refund,
            error,
            contract,
        } = self;
        let obj = JsObject::with_object_proto(ctx.intrinsics());

        let op = op.into_js_object(ctx)?;
        let memory = memory.into_js_object(ctx)?;
        let stack = stack.into_js_object(ctx)?;
        let contract = contract.into_js_object(ctx)?;

        obj.set(js_string!("op"), op, false, ctx)?;
        obj.set(js_string!("memory"), memory, false, ctx)?;
        obj.set(js_string!("stack"), stack, false, ctx)?;
        obj.set(js_string!("contract"), contract, false, ctx)?;

        let error = error.map(|err| JsValue::from(js_string!(err))).unwrap_or_default();
        let get_error = js_value_capture_getter!(error, ctx);
        let get_pc = js_value_getter!(pc, ctx);
        let get_gas = js_value_getter!(gas, ctx);
        let get_cost = js_value_getter!(cost, ctx);
        let get_refund = js_value_getter!(refund, ctx);
        let get_depth = js_value_getter!(depth, ctx);

        obj.set(js_string!("getPC"), get_pc, false, ctx)?;
        obj.set(js_string!("getError"), get_error, false, ctx)?;
        obj.set(js_string!("getGas"), get_gas, false, ctx)?;
        obj.set(js_string!("getCost"), get_cost, false, ctx)?;
        obj.set(js_string!("getDepth"), get_depth, false, ctx)?;
        obj.set(js_string!("getRefund"), get_refund, false, ctx)?;

        Ok(obj)
    }
}

/// Represents the memory object
#[derive(Clone, Debug)]
pub(crate) struct MemoryRef(GuardedNullableGc<Bytes>);

impl MemoryRef {
    /// Creates a new memory reference.
    pub(crate) fn new(mem: &Memory) -> (Self, GcGuard<'_, Bytes>) {
        let bytes = if mem.is_empty() {
            Bytes::new()
        } else {
            Bytes::copy_from_slice(mem.slice(0, mem.len()))
        };
        let (inner, guard) = GuardedNullableGc::new_owned(bytes);
        (Self(inner), guard)
    }

    fn new_bytes(bytes: Bytes) -> (Self, GcGuard<'static, Bytes>) {
        let (inner, guard) = GuardedNullableGc::new_owned(bytes);
        (Self(inner), guard)
    }

    fn len(&self) -> usize {
        self.0.with_inner(|mem| mem.len()).unwrap_or_default()
    }

    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let obj = JsObject::with_object_proto(ctx.intrinsics());
        let len = self.len();

        let length = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| {
                Ok(JsValue::from(len as u64))
            }),
        )
        .length(0)
        .build();

        let slice = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, memory, ctx| {
                    let start = args.get_or_undefined(0).to_numeric_number(ctx)?;
                    let end = args.get_or_undefined(1).to_numeric_number(ctx)?;
                    if end < start || start < 0. || (end as usize) > memory.len() {
                        return Err(JsError::from_native(JsNativeError::typ().with_message(
                            format!(
                                "tracer accessed out of bound memory: offset {start}, end {end}"
                            ),
                        )));
                    }
                    let start = start as usize;
                    let end = end as usize;
                    let slice = memory
                        .0
                        .with_inner(|mem| mem.slice(start..end).to_vec())
                        .unwrap_or_default();

                    to_uint8_array_value(slice, ctx)
                },
                self.clone(),
            ),
        )
        .length(2)
        .build();

        let get_uint = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, memory, ctx| {
                    let offset_f64 = args.get_or_undefined(0).to_numeric_number(ctx)?;
                    let len = memory.len();
                    let offset = offset_f64 as usize;
                    if len < offset + 32 || offset_f64 < 0. {
                        let msg = format!(
                            "tracer accessed out of bound memory: available {len}, offset {offset}, size 32"
                        );
                        return Err(JsError::from_native(JsNativeError::typ().with_message(msg)));
                    }
                    let slice = memory
                        .0
                        .with_inner(|mem| mem.slice(offset..offset + 32).to_vec())
                        .unwrap_or_default();
                    to_uint8_array_value(slice, ctx)
                },
                self,
            ),
        )
        .length(1)
        .build();

        obj.set(js_string!("slice"), slice, false, ctx)?;
        obj.set(js_string!("getUint"), get_uint, false, ctx)?;
        obj.set(js_string!("length"), length, false, ctx)?;
        Ok(obj)
    }
}

impl Finalize for MemoryRef {}

unsafe impl Trace for MemoryRef {
    empty_trace!();
}

/// Represents the opcode object
#[derive(Debug)]
pub(crate) struct OpObj(pub(crate) u8);

impl OpObj {
    pub(crate) fn into_js_object(self, context: &mut Context) -> JsResult<JsObject> {
        let obj = JsObject::with_object_proto(context.intrinsics());
        let value = self.0;
        let is_push = (op::PUSH0..=op::PUSH32).contains(&value);

        let to_number = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| Ok(JsValue::from(value))),
        )
        .length(0)
        .build();

        let is_push = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| Ok(JsValue::from(is_push))),
        )
        .length(0)
        .build();

        let to_string = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| {
                if let Some(op) = OpCode::new(value) {
                    Ok(JsValue::from(js_string!(op.as_str())))
                } else {
                    Ok(JsValue::from(js_string!(format!("opcode {:x} not defined", value))))
                }
            }),
        )
        .length(0)
        .build();

        obj.set(js_string!("toNumber"), to_number, false, context)?;
        obj.set(js_string!("toString"), to_string, false, context)?;
        obj.set(js_string!("isPush"), is_push, false, context)?;
        Ok(obj)
    }
}

impl From<u8> for OpObj {
    fn from(op: u8) -> Self {
        Self(op)
    }
}

/// Represents the stack object
#[derive(Clone, Debug)]
pub(crate) struct StackRef(GuardedNullableGc<Vec<Word>>);

impl StackRef {
    /// Creates a new stack reference.
    pub(crate) fn new(stack: EvmStackRef<'_>) -> (Self, GcGuard<'static, Vec<Word>>) {
        Self::new_words(stack.as_slice().to_vec())
    }

    fn new_words(words: Vec<Word>) -> (Self, GcGuard<'static, Vec<Word>>) {
        let (inner, guard) = GuardedNullableGc::new_owned(words);
        (Self(inner), guard)
    }

    fn peek(&self, idx: usize, ctx: &mut Context) -> JsResult<JsValue> {
        self.0
            .with_inner(|stack| {
                let Some(value) = stack.get(stack.len().saturating_sub(idx + 1)) else {
                    return Err(JsError::from_native(JsNativeError::typ().with_message(format!(
                        "tracer accessed out of bound stack: size {}, index {}",
                        stack.len(),
                        idx
                    ))));
                };
                to_bigint(*value, ctx)
            })
            .ok_or_else(|| {
                JsError::from_native(
                    JsNativeError::typ()
                        .with_message("tracer accessed stack after it was dropped".to_string()),
                )
            })?
    }

    pub(crate) fn into_js_object(self, context: &mut Context) -> JsResult<JsObject> {
        let obj = JsObject::with_object_proto(context.intrinsics());
        let len = self.0.with_inner(|stack| stack.len()).unwrap_or_default();
        let length = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| Ok(JsValue::from(len))),
        )
        .length(0)
        .build();

        let peek = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, stack, ctx| {
                    let idx_f64 = args.get_or_undefined(0).to_numeric_number(ctx)?;
                    let idx = idx_f64 as usize;
                    if len <= idx || idx_f64 < 0. {
                        return Err(JsError::from_native(JsNativeError::typ().with_message(
                            format!("tracer accessed out of bound stack: size {len}, index {idx}"),
                        )));
                    }
                    stack.peek(idx, ctx)
                },
                self,
            ),
        )
        .length(1)
        .build();

        obj.set(js_string!("length"), length, false, context)?;
        obj.set(js_string!("peek"), peek, false, context)?;
        Ok(obj)
    }
}

impl Finalize for StackRef {}

unsafe impl Trace for StackRef {
    empty_trace!();
}

/// Represents the contract object
#[derive(Clone, Debug, Default)]
pub(crate) struct Contract {
    pub(crate) caller: Address,
    pub(crate) contract: Address,
    pub(crate) value: U256,
    pub(crate) input: Bytes,
}

impl Contract {
    /// Converts the contract object into a js object
    ///
    /// Caution: this expects a global property `bigint` to be present.
    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let Self { caller, contract, value, input } = self;
        let obj = JsObject::with_object_proto(ctx.intrinsics());

        let get_caller = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, ctx| {
                address_to_uint8_array_value(caller, ctx)
            }),
        )
        .length(0)
        .build();

        let get_address = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, ctx| {
                address_to_uint8_array_value(contract, ctx)
            }),
        )
        .length(0)
        .build();

        let get_value = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, ctx| to_bigint(value, ctx)),
        )
        .length(0)
        .build();

        let input = to_uint8_array_value(input, ctx)?;
        let get_input = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, _args, input, _ctx| Ok(input.clone()),
                input,
            ),
        )
        .length(0)
        .build();

        obj.set(js_string!("getCaller"), get_caller, false, ctx)?;
        obj.set(js_string!("getAddress"), get_address, false, ctx)?;
        obj.set(js_string!("getValue"), get_value, false, ctx)?;
        obj.set(js_string!("getInput"), get_input, false, ctx)?;

        Ok(obj)
    }
}

/// Represents the call frame object for exit functions
pub(crate) struct FrameResult {
    pub(crate) gas_used: u64,
    pub(crate) output: Bytes,
    pub(crate) error: Option<String>,
}

impl FrameResult {
    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let Self { gas_used, output, error } = self;
        let obj = JsObject::with_object_proto(ctx.intrinsics());

        let output = to_uint8_array_value(output, ctx)?;
        let get_output = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, _args, output, _ctx| Ok(output.clone()),
                output,
            ),
        )
        .length(0)
        .build();

        let error = error.map(|err| JsValue::from(js_string!(err))).unwrap_or_default();
        let get_error = js_value_capture_getter!(error, ctx);
        let get_gas_used = js_value_getter!(gas_used, ctx);

        obj.set(js_string!("getGasUsed"), get_gas_used, false, ctx)?;
        obj.set(js_string!("getOutput"), get_output, false, ctx)?;
        obj.set(js_string!("getError"), get_error, false, ctx)?;

        Ok(obj)
    }
}

/// Represents the call frame object for enter functions
pub(crate) struct CallFrame {
    pub(crate) contract: Contract,
    pub(crate) kind: CallKind,
    pub(crate) gas: u64,
}

impl CallFrame {
    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let Self { contract: Contract { caller, contract, value, input }, kind, gas } = self;
        let obj = JsObject::with_object_proto(ctx.intrinsics());

        let get_from = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, ctx| {
                address_to_uint8_array_value(caller, ctx)
            }),
        )
        .length(0)
        .build();

        let get_to = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, ctx| {
                address_to_uint8_array_value(contract, ctx)
            }),
        )
        .length(0)
        .build();

        let get_value = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure(move |_this, _args, ctx| to_bigint(value, ctx)),
        )
        .length(0)
        .build();

        let input = to_uint8_array_value(input, ctx)?;
        let get_input = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, _args, input, _ctx| Ok(input.clone()),
                input,
            ),
        )
        .length(0)
        .build();

        let get_gas = js_value_getter!(gas, ctx);
        let ty = js_string!(kind.to_string());
        let get_type = js_value_capture_getter!(ty, ctx);

        obj.set(js_string!("getFrom"), get_from, false, ctx)?;
        obj.set(js_string!("getTo"), get_to, false, ctx)?;
        obj.set(js_string!("getValue"), get_value, false, ctx)?;
        obj.set(js_string!("getInput"), get_input, false, ctx)?;
        obj.set(js_string!("getGas"), get_gas, false, ctx)?;
        obj.set(js_string!("getType"), get_type, false, ctx)?;

        Ok(obj)
    }
}

/// The `ctx` object that represents the context in which the transaction is executed.
pub(crate) struct JsEvmContext {
    /// String, one of the two values CALL and CREATE
    pub(crate) r#type: String,
    /// Sender of the transaction
    pub(crate) from: Address,
    /// Target of the transaction
    pub(crate) to: Option<Address>,
    pub(crate) input: Bytes,
    /// Gas limit
    pub(crate) gas: u64,
    /// Number, amount of gas used in executing the transaction (excludes txdata costs)
    pub(crate) gas_used: u64,
    /// Number, gas price configured in the transaction being executed
    pub(crate) gas_price: u64,
    /// Number, intrinsic gas for the transaction being executed
    pub(crate) intrinsic_gas: u64,
    /// big.int Amount to be transferred in wei
    pub(crate) value: U256,
    /// Number, block number
    pub(crate) block: u64,
    /// Address, miner of the block
    pub(crate) coinbase: Address,
    pub(crate) output: Bytes,
    /// Number, block timestamp
    pub(crate) time: String,
    pub(crate) transaction_ctx: TransactionContext,
    /// returns information about the error if one occurred, otherwise returns undefined
    pub(crate) error: Option<String>,
}

impl JsEvmContext {
    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let Self {
            r#type,
            from,
            to,
            input,
            gas,
            gas_used,
            gas_price,
            intrinsic_gas,
            value,
            block,
            coinbase,
            output,
            time,
            transaction_ctx,
            error,
        } = self;
        let obj = JsObject::with_object_proto(ctx.intrinsics());

        obj.set(js_string!("type"), js_string!(r#type), false, ctx)?;
        obj.set(js_string!("from"), address_to_uint8_array(from, ctx)?, false, ctx)?;
        if let Some(to) = to {
            obj.set(js_string!("to"), address_to_uint8_array(to, ctx)?, false, ctx)?;
        } else {
            obj.set(js_string!("to"), JsValue::null(), false, ctx)?;
        }

        obj.set(js_string!("input"), to_uint8_array(input, ctx)?, false, ctx)?;
        obj.set(js_string!("gas"), gas, false, ctx)?;
        obj.set(js_string!("gasUsed"), gas_used, false, ctx)?;
        obj.set(js_string!("gasPrice"), gas_price, false, ctx)?;
        obj.set(js_string!("intrinsicGas"), intrinsic_gas, false, ctx)?;
        obj.set(js_string!("value"), to_bigint(value, ctx)?, false, ctx)?;
        obj.set(js_string!("block"), block, false, ctx)?;
        obj.set(js_string!("coinbase"), address_to_uint8_array(coinbase, ctx)?, false, ctx)?;
        obj.set(js_string!("output"), to_uint8_array(output, ctx)?, false, ctx)?;
        obj.set(js_string!("time"), js_string!(time), false, ctx)?;
        if let Some(block_hash) = transaction_ctx.block_hash {
            obj.set(js_string!("blockHash"), to_uint8_array(block_hash, ctx)?, false, ctx)?;
        }
        if let Some(tx_index) = transaction_ctx.tx_index {
            obj.set(js_string!("txIndex"), tx_index as u64, false, ctx)?;
        }
        if let Some(tx_hash) = transaction_ctx.tx_hash {
            obj.set(js_string!("txHash"), to_uint8_array(tx_hash, ctx)?, false, ctx)?;
        }
        if let Some(error) = error {
            obj.set(js_string!("error"), js_string!(error), false, ctx)?;
        }

        Ok(obj)
    }
}

/// DB is the object that allows the js inspector to interact with the database.
#[derive(Clone, Debug)]
pub(crate) struct EvmDbRef {
    db: GuardedNullableGc<CacheDB<EmptyDB>>,
}

impl EvmDbRef {
    /// Creates a new evm and db JS object.
    pub(crate) fn new(db: &CacheDB<EmptyDB>) -> (Self, EvmDbGuard<'_>) {
        let (db, db_guard) = GuardedNullableGc::new_ref(db);
        (Self { db }, EvmDbGuard { _db_guard: db_guard })
    }

    fn read_basic(&self, address: JsValue, ctx: &mut Context) -> JsResult<Option<AccountInfo>> {
        let buf = bytes_from_value(address, ctx)?;
        let address = bytes_to_address(&buf);
        self.db.with_inner(|db| db.account_info(&address).cloned()).ok_or_else(|| {
            JsError::from_native(
                JsNativeError::error()
                    .with_message(format!("Failed to read address {address:?} from database")),
            )
        })
    }

    fn read_code(&self, address: JsValue, ctx: &mut Context) -> JsResult<JsUint8Array> {
        let acc = self.read_basic(address, ctx)?;
        let code_hash = acc.as_ref().map(|acc| acc.code_hash).unwrap_or(KECCAK256_EMPTY);
        let bytes = self
            .db
            .with_inner(|db| db.cache.contracts.get(&code_hash).map(|code| code.original_bytes()))
            .flatten()
            .unwrap_or_default();
        to_uint8_array(bytes, ctx)
    }

    fn read_state(
        &self,
        address: JsValue,
        slot: JsValue,
        ctx: &mut Context,
    ) -> JsResult<JsUint8Array> {
        let buf = bytes_from_value(address, ctx)?;
        let address = bytes_to_address(&buf);
        let buf = bytes_from_value(slot, ctx)?;
        let slot: U256 = bytes_to_b256(&buf).into();

        let value = self
            .db
            .with_inner(|db| db.cache.storage.get(&evm2::StorageKey::new(address, slot)).copied())
            .ok_or_else(|| {
                JsError::from_native(JsNativeError::error().with_message(format!(
                    "Failed to read state for {address:?} at {slot:?} from database",
                )))
            })?
            .unwrap_or_default();
        to_uint8_array(B256::from(value), ctx)
    }

    pub(crate) fn into_js_object(self, ctx: &mut Context) -> JsResult<JsObject> {
        let obj = JsObject::with_object_proto(ctx.intrinsics());
        let exists = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, db, ctx| {
                    let val = args.get_or_undefined(0).clone();
                    let acc = db.read_basic(val, ctx)?;
                    Ok(JsValue::from(acc.is_some()))
                },
                self.clone(),
            ),
        )
        .length(1)
        .build();

        let get_balance = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, db, ctx| {
                    let val = args.get_or_undefined(0).clone();
                    let acc = db.read_basic(val, ctx)?;
                    let balance = acc.map(|acc| acc.balance).unwrap_or_default();
                    to_bigint(balance, ctx)
                },
                self.clone(),
            ),
        )
        .length(1)
        .build();

        let get_nonce = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, db, ctx| {
                    let val = args.get_or_undefined(0).clone();
                    let acc = db.read_basic(val, ctx)?;
                    let nonce = acc.map(|acc| acc.nonce).unwrap_or_default();
                    Ok(JsValue::from(nonce))
                },
                self.clone(),
            ),
        )
        .length(1)
        .build();

        let get_code = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, db, ctx| {
                    let val = args.get_or_undefined(0).clone();
                    Ok(db.read_code(val, ctx)?.into())
                },
                self.clone(),
            ),
        )
        .length(1)
        .build();

        let get_state = FunctionObjectBuilder::new(
            ctx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_this, args, db, ctx| {
                    let addr = args.get_or_undefined(0).clone();
                    let slot = args.get_or_undefined(1).clone();
                    Ok(db.read_state(addr, slot, ctx)?.into())
                },
                self,
            ),
        )
        .length(2)
        .build();

        obj.set(js_string!("getBalance"), get_balance, false, ctx)?;
        obj.set(js_string!("getNonce"), get_nonce, false, ctx)?;
        obj.set(js_string!("getCode"), get_code, false, ctx)?;
        obj.set(js_string!("getState"), get_state, false, ctx)?;
        obj.set(js_string!("exists"), exists, false, ctx)?;
        Ok(obj)
    }
}

impl Finalize for EvmDbRef {}

unsafe impl Trace for EvmDbRef {
    empty_trace!();
}

#[must_use]
pub(crate) struct EvmDbGuard<'a> {
    _db_guard: GcGuard<'a, CacheDB<EmptyDB>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::js::builtins::{json_stringify, register_builtins, to_serde_value};
    use alloc::vec;
    use boa_engine::Source;

    #[test]
    fn test_contract() {
        let mut ctx = Context::default();
        let contract = Contract {
            caller: Address::ZERO,
            contract: Address::ZERO,
            value: U256::from(1337u64),
            input: vec![0x01, 0x02, 0x03].into(),
        };
        register_builtins(&mut ctx).unwrap();

        let obj = contract.clone().into_js_object(&mut ctx).unwrap();
        let s = "({
                caller: function(contract) { return contract.getCaller(); },
                value: function(contract) { return contract.getValue(); },
                address: function(contract) { return contract.getAddress(); },
                input: function(contract) { return contract.getInput(); }
        })";

        let contract_arg = JsValue::from(obj);
        let eval_obj = ctx.eval(Source::from_bytes(s)).unwrap();
        let call = eval_obj.as_object().unwrap().get(js_string!("caller"), &mut ctx).unwrap();
        let res = call
            .as_callable()
            .unwrap()
            .call(&JsValue::undefined(), core::slice::from_ref(&contract_arg), &mut ctx)
            .unwrap();
        assert!(res.is_object());
        let obj = res.as_object().unwrap();
        let array_buf = JsUint8Array::from_object(obj);
        assert!(array_buf.is_ok());

        let get_address =
            eval_obj.as_object().unwrap().get(js_string!("address"), &mut ctx).unwrap();
        let res = get_address
            .as_callable()
            .unwrap()
            .call(&JsValue::undefined(), core::slice::from_ref(&contract_arg), &mut ctx)
            .unwrap();
        assert!(res.is_object());

        let buf = bytes_from_value(res, &mut ctx).unwrap();
        assert_eq!(buf, contract.contract.as_slice());

        let call = eval_obj.as_object().unwrap().get(js_string!("value"), &mut ctx).unwrap();
        let res = call
            .as_callable()
            .unwrap()
            .call(&JsValue::undefined(), core::slice::from_ref(&contract_arg), &mut ctx)
            .unwrap();
        assert_eq!(
            res.to_string(&mut ctx).unwrap().to_std_string().unwrap(),
            contract.value.to_string()
        );

        let call = eval_obj.as_object().unwrap().get(js_string!("input"), &mut ctx).unwrap();
        let res = call
            .as_callable()
            .unwrap()
            .call(&JsValue::undefined(), &[contract_arg], &mut ctx)
            .unwrap();

        let buf = bytes_from_value(res, &mut ctx).unwrap();
        assert_eq!(buf, contract.input);
    }

    #[test]
    fn test_evm_db_gc() {
        let mut context = Context::default();

        let result = context
            .eval(Source::from_bytes(
                "(
                    function(db, addr) {return db.exists(addr) }
            )
        "
                .to_string()
                .as_bytes(),
            ))
            .unwrap();
        assert!(result.is_callable());

        let f = result.as_callable().unwrap();

        let mut db = CacheDB::new(EmptyDB::default());
        {
            let (db, guard) = EvmDbRef::new(&db);
            let addr = Address::default();
            let addr = JsValue::from(js_string!(addr.to_string()));
            let db = db.into_js_object(&mut context).unwrap();
            let res = f.call(&result, &[db.clone().into(), addr.clone()], &mut context).unwrap();
            assert!(!res.as_boolean().unwrap());

            drop(guard);
            let res = f.call(&result, &[db.into(), addr], &mut context);
            assert!(res.is_err());
        }
        let addr = Address::default();
        db.insert_account_info(&addr, Default::default());

        {
            let (db, guard) = EvmDbRef::new(&db);
            let addr = JsValue::from(js_string!(addr.to_string()));
            let db = db.into_js_object(&mut context).unwrap();
            let res = f.call(&result, &[db.clone().into(), addr.clone()], &mut context).unwrap();

            assert!(res.as_boolean().unwrap());

            drop(guard);
            let res = f.call(&result, &[db.into(), addr], &mut context);
            assert!(res.is_err());
        }
    }

    #[test]
    fn test_evm_db_gc_captures() {
        let mut context = Context::default();

        let res = context
            .eval(Source::from_bytes(
                r"({
                 setup: function(db) {this.db = db;},
                 result: function(addr) {return this.db.exists(addr) }
            })
        "
                .to_string()
                .as_bytes(),
            ))
            .unwrap();

        let obj = res.as_object().unwrap();

        let result_fn = obj.get(js_string!("result"), &mut context).unwrap().as_object().unwrap();
        let setup_fn = obj.get(js_string!("setup"), &mut context).unwrap().as_object().unwrap();

        let db = CacheDB::new(EmptyDB::default());
        {
            let (db_ref, guard) = EvmDbRef::new(&db);
            let js_db = db_ref.into_js_object(&mut context).unwrap();
            let _res = setup_fn.call(&(obj.clone().into()), &[js_db.into()], &mut context).unwrap();
            assert!(obj.get(js_string!("db"), &mut context).unwrap().is_object());

            let addr = Address::default();
            let addr = JsValue::from(js_string!(addr.to_string()));
            let res = result_fn
                .call(&(obj.clone().into()), core::slice::from_ref(&addr), &mut context)
                .unwrap();
            assert!(!res.as_boolean().unwrap());

            drop(guard);
            let res = result_fn.call(&(obj.clone().into()), &[addr], &mut context);
            assert!(res.is_err());
        }
    }

    #[test]
    fn test_big_int() {
        let mut context = Context::default();
        register_builtins(&mut context).unwrap();

        let eval = context
            .eval(Source::from_bytes(
                r#"({data: [], fault: function(log) {}, step: function(log) { this.data.push({ value: log.stack.peek(2) }) }, result: function() { return this.data; }})"#
                .to_string()
                .as_bytes(),
            ))
            .unwrap();

        let obj = eval.as_object().unwrap();

        let result_fn = obj.get(js_string!("result"), &mut context).unwrap().as_object().unwrap();
        let step_fn = obj.get(js_string!("step"), &mut context).unwrap().as_object().unwrap();

        let (stack_ref, _stack_guard) =
            StackRef::new_words(vec![U256::from(35000), U256::from(35000), U256::from(35000)]);
        let (mem_ref, _mem_guard) = MemoryRef::new_bytes(Bytes::new());

        let step = StepLog {
            stack: stack_ref,
            op: OpObj(0),
            memory: mem_ref,
            pc: 0,
            gas_remaining: 0,
            cost: 0,
            depth: 0,
            refund: 0,
            error: None,
            contract: Default::default(),
        };

        let js_step = step.into_js_object(&mut context).unwrap();

        let _ = step_fn.call(&eval, &[js_step.into()], &mut context).unwrap();

        let res = result_fn.call(&eval, &[], &mut context).unwrap();
        let val = json_stringify(res.clone(), &mut context).unwrap().to_std_string().unwrap();
        assert_eq!(val, r#"[{"value":"35000"}]"#);

        let val = to_serde_value(res, &mut context).unwrap();
        assert!(val.is_array());
        let s = val.to_string();
        assert_eq!(s, r#"[{"value":"35000"}]"#);
    }

    #[test]
    fn test_object_functions() {
        let mut context = Context::default();
        register_builtins(&mut context).unwrap();

        let eval = context
            .eval(Source::from_bytes(
                r#"(
    {
        retVal: [],
        callStack: [],
        byte2Hex: function (byte) {
            if (byte < 0x10) return "0" + byte.toString(16);
            return byte.toString(16);
        },
        array2Hex: function (arr) {
            var retVal = "";
            for (var i = 0; i < arr.length; i++) retVal += this.byte2Hex(arr[i]);
            return retVal;
        },
        getAddr: function (log) {
            return this.array2Hex(log.contract.getAddress());
        },
        step: function (log, db) {
            var opcode = log.op.toNumber();
            if (opcode == 0x54) {
                this.retVal.push(this.getAddr(log) + ":" + log.stack.peek(0).toString(16));
            }
            if (opcode == 0x55)
                this.retVal.push(
                    this.getAddr(log) +
                        ":" +
                        log.stack.peek(0).toString(16) +
                        ";" +
                        log.stack.peek(1).toString(16)
                );
        },
        fault: function (log, db) {
            this.retVal.push("FAULT: ");
        },
        result: function (ctx, db) {
            return this.retVal;
        },
   }
)"#
                .to_string()
                .as_bytes(),
            ))
            .unwrap();

        let obj = eval.as_object().unwrap();

        let result_fn = obj.get(js_string!("result"), &mut context).unwrap().as_object().unwrap();
        let step_fn = obj.get(js_string!("step"), &mut context).unwrap().as_object().unwrap();

        let (stack_ref, _stack_guard) =
            StackRef::new_words(vec![U256::from(35000), U256::from(35000), U256::from(35000)]);
        let (mem_ref, _mem_guard) = MemoryRef::new_bytes(Bytes::new());

        let step = StepLog {
            stack: stack_ref,
            op: OpObj(85),
            memory: mem_ref,
            pc: 0,
            gas_remaining: 0,
            cost: 0,
            depth: 0,
            refund: 0,
            error: None,
            contract: Default::default(),
        };

        let js_step = step.into_js_object(&mut context).unwrap();

        let _ = step_fn.call(&eval, &[js_step.into()], &mut context).unwrap();

        let res = result_fn.call(&eval, &[], &mut context).unwrap();
        let val = json_stringify(res, &mut context).unwrap().to_std_string().unwrap();
        assert_eq!(val, r#"["0000000000000000000000000000000000000000:88b8;88b8"]"#);
    }
}
