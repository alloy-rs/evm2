//! evm2-facing runtime context.

use crate::{CallInput, EvmStack, EvmWord, HostContext, Inputs, InstrStop};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::Bytes;
use core::{
    cmp::min,
    fmt,
    ops::Range,
    ptr::{self, NonNull},
};
use evm2::{
    BaseEvmTypes, SpecId,
    bytecode::Bytecode,
    env::TxEnv,
    interpreter::{
        Gas as Evm2Gas, Host as Evm2Host, Interpreter, Memory, Message, MessageKind, Word,
    },
    version::{GasId, GasParams},
};

const _: () = {
    assert!(core::mem::size_of::<EvmWord>() == core::mem::size_of::<Word>());
    assert!(core::mem::align_of::<EvmWord>() == core::mem::align_of::<Word>());
};

/// The evm2 bytecode compiler runtime context.
#[repr(C)]
pub struct EvmContext<'a> {
    /// The memory.
    pub memory: *mut Memory,
    /// Input information (target address, caller, input data, call value).
    pub input: *mut Inputs,
    /// The gas.
    pub gas: Evm2Gas,
    /// Host state consumed by host-touching builtins.
    pub host: HostContext<'a>,
    /// The return data.
    pub return_data: &'a [u8],
    /// Whether the context is static.
    pub is_static: bool,
    /// The spec ID for the current execution.
    pub spec_id: SpecId,
    /// The contract bytecode, for CODECOPY at runtime.
    pub bytecode: *const [u8],
    /// Optional callback invoked by the LOG builtin after constructing the log.
    #[doc(hidden)]
    pub on_log: Option<&'a mut (dyn FnMut(&alloy_primitives::Log) + 'a)>,
    /// The size of the call input data, cached for CALLDATASIZE.
    pub calldatasize: usize,
    /// The result set by a builtin before exiting via `evm2_jit_exit`.
    pub exit_result: InstrStop,
    /// Saved RSP from the entry trampoline, used by `evm2_jit_exit` to unwind.
    pub exit_sp: *mut u8,
    /// Cached gas parameters for builtin gas accounting.
    pub gas_params: GasParams,
    /// Cached base pointer for the current memory context.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    pub mem_len: usize,
    /// Output produced by RETURN or REVERT.
    #[doc(hidden)]
    pub output: Bytes,
    /// Transaction-global environment.
    #[doc(hidden)]
    pub tx_env: &'a TxEnv<BaseEvmTypes>,
    /// Frame-local call/create message.
    #[doc(hidden)]
    pub message: &'a Message<BaseEvmTypes>,
    return_data_scratch: Bytes,
    memory_scratch: Box<Memory>,
    input_scratch: Box<Inputs>,
}

const _: () = {
    use core::mem::offset_of;

    assert!(offset_of!(EvmContext<'_>, memory) == offset_of!(crate::EvmContext<'_>, memory));
    assert!(offset_of!(EvmContext<'_>, input) == offset_of!(crate::EvmContext<'_>, input));
    assert!(offset_of!(EvmContext<'_>, gas) == offset_of!(crate::EvmContext<'_>, gas));
    assert!(offset_of!(EvmContext<'_>, host) == offset_of!(crate::EvmContext<'_>, host));
    assert!(
        offset_of!(EvmContext<'_>, return_data) == offset_of!(crate::EvmContext<'_>, return_data)
    );
    assert!(offset_of!(EvmContext<'_>, is_static) == offset_of!(crate::EvmContext<'_>, is_static));
    assert!(offset_of!(EvmContext<'_>, spec_id) == offset_of!(crate::EvmContext<'_>, spec_id));
    assert!(offset_of!(EvmContext<'_>, bytecode) == offset_of!(crate::EvmContext<'_>, bytecode));
    assert!(offset_of!(EvmContext<'_>, on_log) == offset_of!(crate::EvmContext<'_>, on_log));
    assert!(
        offset_of!(EvmContext<'_>, calldatasize) == offset_of!(crate::EvmContext<'_>, calldatasize)
    );
    assert!(
        offset_of!(EvmContext<'_>, exit_result) == offset_of!(crate::EvmContext<'_>, exit_result)
    );
    assert!(offset_of!(EvmContext<'_>, exit_sp) == offset_of!(crate::EvmContext<'_>, exit_sp));
    assert!(
        offset_of!(EvmContext<'_>, gas_params) == offset_of!(crate::EvmContext<'_>, gas_params)
    );
    assert!(offset_of!(EvmContext<'_>, mem_base) == offset_of!(crate::EvmContext<'_>, mem_base));
    assert!(offset_of!(EvmContext<'_>, mem_len) == offset_of!(crate::EvmContext<'_>, mem_len));
    assert!(offset_of!(EvmContext<'_>, output) == offset_of!(crate::EvmContext<'_>, output));
};

/// Interpreter state copied out of a JIT context after compiled execution.
#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct InterpreterState {
    gas: Evm2Gas,
    return_data: Vec<u8>,
    memory: Vec<u8>,
    output: Option<Vec<u8>>,
}

impl InterpreterState {
    /// Stores this state back into an evm2 interpreter.
    #[inline]
    pub fn store(self, interpreter: &mut Interpreter<'_, BaseEvmTypes>) {
        interpreter.set_gas(self.gas);
        interpreter.set_return_data(self.return_data.into());
        let memory = interpreter.memory_mut();
        memory.clear();
        memory.resize(0, self.memory.len()).expect("JIT memory snapshot exceeds evm2 memory limit");
        memory.set(0, &self.memory);
        if let Some(output) = self.output {
            interpreter.set_output_bytes_for_jit(&output);
        }
    }
}

/// An evm2 bytecode function.
pub type EvmCompilerFn = crate::EvmCompilerFn;

impl crate::EvmCompilerFn {
    /// Calls the function by re-using an evm2 interpreter's resources.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the function is safe to call for this interpreter state.
    pub unsafe fn call_with_interpreter<'a, 'frame: 'a>(
        self,
        interpreter: &'a mut Interpreter<'frame, BaseEvmTypes>,
        host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    ) -> InstrStop {
        let (mut ecx, stack, stack_len) =
            EvmContext::from_interpreter_with_stack(interpreter, host);
        let result = unsafe { self.call_with_evm2_context(stack, stack_len, &mut ecx) };
        if result == InstrStop::OutOfGas {
            ecx.gas.spend_all();
        }

        let mut state = ecx.interpreter_state();
        if matches!(result, InstrStop::Return | InstrStop::Revert) {
            state.output = Some(ecx.output.to_vec());
        }
        drop(ecx);
        state.store(interpreter);
        result
    }

    /// Calls the function with an evm2-facing context.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the arguments are valid and that the function is safe to call.
    #[inline]
    pub unsafe fn call_with_evm2_context(
        self,
        stack: &mut EvmStack,
        stack_len: &mut usize,
        ecx: &mut EvmContext<'_>,
    ) -> InstrStop {
        let ecx = unsafe {
            NonNull::new_unchecked((ecx as *mut EvmContext<'_>).cast::<crate::EvmContext<'_>>())
        };
        unsafe {
            crate::evm2_jit_entry(
                ecx,
                NonNull::from(stack),
                NonNull::from(stack_len),
                self.into_inner(),
            )
        }
    }
}

impl fmt::Debug for EvmContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory).finish_non_exhaustive()
    }
}

impl<'a> EvmContext<'a> {
    /// Creates a new context from an interpreter.
    #[inline]
    pub fn from_interpreter<'frame: 'a>(
        interpreter: &'a mut Interpreter<'frame, BaseEvmTypes>,
        host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    ) -> Self {
        Self::from_interpreter_with_stack(interpreter, host).0
    }

    /// Creates a new context from an interpreter and returns the borrowed stack.
    #[inline]
    pub fn from_interpreter_with_stack<'frame: 'a>(
        interpreter: &'a mut Interpreter<'frame, BaseEvmTypes>,
        host: &'a mut (dyn Evm2Host<BaseEvmTypes> + 'a),
    ) -> (Self, &'a mut EvmStack, &'a mut usize) {
        let version = *interpreter.version();
        let tx_env = interpreter.tx_env();
        let message = interpreter.message();
        let gas = interpreter.gas();
        let return_data_scratch = interpreter.return_data().clone();
        let memory_bytes = interpreter.memory_ref().as_slice();
        let mut memory_scratch = Box::new(Memory::new());
        memory_scratch.set_memory_limit(version.memory_limit);
        memory_scratch
            .resize(0, memory_bytes.len())
            .expect("JIT memory snapshot exceeds evm2 memory limit");
        memory_scratch.set(0, memory_bytes);
        let bytecode = interpreter.bytecode().as_slice() as *const [u8];
        let spec_id = interpreter.spec();
        let is_static = interpreter.is_static();
        let (stack_ptr, stack_len) = interpreter.stack_mut().into_raw_parts();
        let stack = unsafe { EvmStack::from_mut_ptr(stack_ptr.cast()) };
        let memory = memory_scratch.as_mut() as *mut Memory;
        let calldatasize = message.input.len();
        let mut input_scratch = Box::new(Inputs {
            target_address: message.destination,
            bytecode_address: Some(message.code_address),
            caller_address: message.caller,
            input: CallInput::Bytes(Bytes::copy_from_slice(message.input.as_ref())),
            call_value: message.value,
        });
        let input = input_scratch.as_mut() as *mut Inputs;
        let host = HostContext::new(host, tx_env, &version);
        let return_data = unsafe { &*(return_data_scratch.as_ref() as *const [u8]) };
        let mut this = Self {
            memory,
            input,
            gas,
            host,
            return_data,
            is_static,
            spec_id,
            bytecode,
            on_log: None,
            calldatasize,
            exit_result: InstrStop::Stop,
            exit_sp: ptr::null_mut(),
            gas_params: version.gas_params,
            mem_base: ptr::null_mut(),
            mem_len: 0,
            output: Bytes::new(),
            tx_env,
            message,
            return_data_scratch,
            memory_scratch,
            input_scratch,
        };
        this.refresh_memory_cache();
        (this, stack, stack_len)
    }

    /// Returns the context state that must be copied back into an interpreter.
    #[inline]
    pub fn interpreter_state(&self) -> InterpreterState {
        InterpreterState {
            gas: self.gas,
            return_data: self.return_data.to_vec(),
            memory: unsafe { &*self.memory }.as_slice().to_vec(),
            output: None,
        }
    }

    /// Stores context state back into an interpreter after compiled execution.
    #[inline]
    pub fn store_interpreter_state(self, interpreter: &mut Interpreter<'_, BaseEvmTypes>) {
        self.interpreter_state().store(interpreter);
    }

    /// Refreshes the cached memory base pointer and length from the evm2 memory snapshot.
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let slice = unsafe { &mut *self.memory }.as_mut_slice();
        self.mem_base = slice.as_mut_ptr();
        self.mem_len = slice.len();
    }

    /// Returns the input shim visible to compiled code.
    #[inline]
    pub fn input(&self) -> &Inputs {
        &self.input_scratch
    }

    fn set_return_data(&mut self, data: Bytes) {
        self.return_data_scratch = data;
        self.return_data = unsafe { &*(self.return_data_scratch.as_ref() as *const [u8]) };
    }
}

/// Returns the bytecode bytes for CODECOPY-compatible runtime access.
#[inline]
pub fn bytecode_slice(bytecode: &Bytecode) -> &[u8] {
    bytecode.original_byte_slice()
}

#[doc(hidden)]
pub unsafe fn execute_call_message(
    ecx: &mut crate::EvmContext<'_>,
    sp: *mut EvmWord,
    call_kind: u8,
) -> Result<(), InstrStop> {
    let ecx = unsafe { evm2_context_from_base(ecx) };
    call_kind_from_u8(call_kind).and_then(|kind| call_inner(ecx, sp, kind))
}

#[doc(hidden)]
pub unsafe fn execute_create_message(
    ecx: &mut crate::EvmContext<'_>,
    sp: *mut EvmWord,
    create_kind: u8,
) -> Result<(), InstrStop> {
    let ecx = unsafe { evm2_context_from_base(ecx) };
    create_kind_from_u8(create_kind).and_then(|is_create2| create_inner(ecx, sp, is_create2))
}

unsafe fn evm2_context_from_base<'a, 'ctx>(
    ecx: &'a mut crate::EvmContext<'ctx>,
) -> &'a mut EvmContext<'ctx> {
    unsafe { &mut *(ptr::from_mut(ecx).cast::<EvmContext<'ctx>>()) }
}

fn call_kind_from_u8(kind: u8) -> Result<MessageKind, InstrStop> {
    match kind {
        0 => Ok(MessageKind::Call),
        1 => Ok(MessageKind::CallCode),
        2 => Ok(MessageKind::DelegateCall),
        3 => Ok(MessageKind::StaticCall),
        _ => Err(InstrStop::FatalExternalError),
    }
}

fn create_kind_from_u8(kind: u8) -> Result<bool, InstrStop> {
    match kind {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(InstrStop::FatalExternalError),
    }
}

fn word_to_usize(value: EvmWord) -> Result<usize, InstrStop> {
    value.to_u256().try_into().map_err(|_| InstrStop::InvalidOperandOOG)
}

fn word_to_usize_saturated(value: EvmWord) -> usize {
    value.to_u256().try_into().unwrap_or(usize::MAX)
}

fn ensure_memory(ecx: &mut EvmContext<'_>, offset: usize, len: usize) -> Result<(), InstrStop> {
    unsafe { &mut *ecx.memory }.resize_evm(&mut ecx.gas, offset, len)?;
    ecx.refresh_memory_cache();
    Ok(())
}

fn resize_evm_range(
    ecx: &mut EvmContext<'_>,
    offset: EvmWord,
    len: EvmWord,
) -> Result<Range<usize>, InstrStop> {
    let len = word_to_usize(len)?;
    let offset = if len != 0 {
        let offset = word_to_usize(offset)?;
        ensure_memory(ecx, offset, len)?;
        offset
    } else {
        usize::MAX
    };
    Ok(offset..offset + len)
}

fn memory_range_bytes(ecx: &mut EvmContext<'_>, range: Range<usize>) -> Bytes {
    if range.is_empty() {
        return Bytes::new();
    }
    Bytes::copy_from_slice(unsafe { &*ecx.memory }.slice(range.start, range.len()))
}

fn get_memory_input_and_out_ranges(
    ecx: &mut EvmContext<'_>,
    input_offset: EvmWord,
    input_len: EvmWord,
    return_offset: EvmWord,
    return_len: EvmWord,
) -> Result<(Range<usize>, Range<usize>), InstrStop> {
    let input = resize_evm_range(ecx, input_offset, input_len)?;
    let output = resize_evm_range(ecx, return_offset, return_len)?;
    Ok((input, output))
}

fn spend(gas: &mut Evm2Gas, cost: u64) -> Result<(), InstrStop> {
    gas.spend(cost)
}

fn should_charge_new_account_gas(
    eip161: bool,
    transfers_value: bool,
    target_is_empty_for_new_account_gas: bool,
) -> bool {
    target_is_empty_for_new_account_gas && (!eip161 || transfers_value)
}

fn load_acc_and_calc_gas(
    ecx: &mut EvmContext<'_>,
    to: alloy_primitives::Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, Bytecode, alloy_primitives::Address, bool), InstrStop> {
    let version = *ecx.host.version();
    if transfers_value {
        spend(&mut ecx.gas, version.gas_params.get(GasId::TransferValueCost).into())?;
    }

    let additional_cold_cost = version.gas_params.cold_account_additional_cost();
    let remaining_gas = ecx.gas.remaining();
    let skip_cold_load = remaining_gas < additional_cold_cost;
    let account = ecx.host.host_mut().load_account(&to, true, skip_cold_load)?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    let mut code = account.code;
    let mut code_address = to;
    if ecx.spec_id.enables(SpecId::PRAGUE)
        && let Some(delegated_address) = code.eip7702_address()
    {
        cost += u64::from(version.gas_params.get(GasId::WarmStorageReadCost));
        if cost > remaining_gas {
            return Err(InstrStop::OutOfGas);
        }
        let skip_cold_load = remaining_gas < cost.saturating_add(additional_cold_cost);
        let delegated_account =
            ecx.host.host_mut().load_account(&delegated_address, true, skip_cold_load)?;
        if delegated_account.is_cold {
            cost += additional_cold_cost;
        }
        code = delegated_account.code;
        code_address = delegated_address;
    }
    if create_empty_account
        && should_charge_new_account_gas(
            ecx.spec_id.enables(SpecId::SPURIOUS_DRAGON),
            transfers_value,
            ecx.host.host_mut().target_is_empty_for_new_account_gas(&to, ecx.spec_id)?,
        )
    {
        cost += u64::from(version.gas_params.get(GasId::NewAccountCost));
    }
    spend(&mut ecx.gas, cost)?;

    let mut gas_limit = if ecx.spec_id.enables(SpecId::TANGERINE) {
        min(version.gas_params.call_stipend_reduction(ecx.gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    spend(&mut ecx.gas, gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(version.gas_params.get(GasId::CallStipend).into());
    }

    let disable_precompiles = code_address != to;
    Ok((gas_limit, code, code_address, disable_precompiles))
}

unsafe fn pop_word(sp: &mut *mut EvmWord) -> EvmWord {
    *sp = unsafe { (*sp).sub(1) };
    unsafe { **sp }
}

fn call_inner(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    kind: MessageKind,
) -> Result<(), InstrStop> {
    let inputs = match kind {
        MessageKind::Call | MessageKind::CallCode => 7,
        MessageKind::DelegateCall | MessageKind::StaticCall => 6,
        _ => unreachable!("invalid call message kind"),
    };
    let mut cursor = unsafe { sp.add(inputs) };
    let local_gas_limit = unsafe { pop_word(&mut cursor) };
    let to = unsafe { pop_word(&mut cursor) }.to_address();
    let value = if matches!(kind, MessageKind::Call | MessageKind::CallCode) {
        unsafe { pop_word(&mut cursor) }.to_u256()
    } else {
        Word::ZERO
    };
    let input_offset = unsafe { pop_word(&mut cursor) };
    let input_len = unsafe { pop_word(&mut cursor) };
    let return_offset = unsafe { pop_word(&mut cursor) };
    let return_len = unsafe { pop_word(&mut cursor) };

    let has_transfer = !value.is_zero();
    if ecx.is_static && kind == MessageKind::Call && has_transfer {
        return Err(InstrStop::CallNotAllowedInsideStatic);
    }

    let local_gas_limit = word_to_usize_saturated(local_gas_limit) as u64;
    let (input_range, return_memory_range) =
        get_memory_input_and_out_ranges(ecx, input_offset, input_len, return_offset, return_len)?;
    let (gas_limit, loaded_code, resolved_code_address, disable_precompiles) =
        load_acc_and_calc_gas(ecx, to, has_transfer, kind == MessageKind::Call, local_gas_limit)?;
    let input = memory_range_bytes(ecx, input_range);

    let current = ecx.message;
    let (destination, caller, call_value, code_address) = match kind {
        MessageKind::Call => (to, current.destination, value, resolved_code_address),
        MessageKind::CallCode => {
            (current.destination, current.destination, value, resolved_code_address)
        }
        MessageKind::DelegateCall => {
            (current.destination, current.caller, current.value, resolved_code_address)
        }
        MessageKind::StaticCall => (to, current.destination, Word::ZERO, resolved_code_address),
        _ => unreachable!("invalid call message kind"),
    };
    let mut message = Message {
        kind,
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination,
        caller,
        input,
        value: call_value,
        code_address,
        disable_precompiles,
        salt: alloy_primitives::B256::ZERO,
        ext: (),
        _non_exhaustive: (),
    };

    let caller_is_static = ecx.is_static;
    let mut result =
        ecx.host.execute_message(ecx.tx_env, loaded_code, &mut message, caller_is_static);
    ecx.gas.erase_cost(result.gas_returned_to_parent());
    ecx.gas.record_refund(result.refund_propagated_to_parent());

    let copy_len = min(return_memory_range.len(), result.output.len());
    if copy_len != 0 {
        unsafe { &mut *ecx.memory }.set(return_memory_range.start, &result.output[..copy_len]);
    }
    let success = EvmWord::from(Word::from(u8::from(result.stop.is_success())));
    unsafe {
        sp.write(success);
    }
    ecx.set_return_data(core::mem::take(&mut result.output));
    Ok(())
}

fn create_inner(
    ecx: &mut EvmContext<'_>,
    sp: *mut EvmWord,
    is_create2: bool,
) -> Result<(), InstrStop> {
    if ecx.is_static {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }

    let inputs = if is_create2 { 4 } else { 3 };
    let mut cursor = unsafe { sp.add(inputs) };
    let value = unsafe { pop_word(&mut cursor) }.to_u256();
    let offset = unsafe { pop_word(&mut cursor) };
    let len_word = unsafe { pop_word(&mut cursor) };
    let salt = if is_create2 { Some(unsafe { pop_word(&mut cursor) }) } else { None };

    let len = word_to_usize(len_word)?;
    let version = *ecx.host.version();
    if ecx.spec_id.enables(SpecId::SHANGHAI) {
        if len > version.max_initcode_size {
            return Err(InstrStop::CreateInitCodeSizeLimit);
        }
        spend(&mut ecx.gas, version.gas_params.initcode_cost(len))?;
    }
    let code_range = resize_evm_range(ecx, offset, EvmWord::from(Word::from(len)))?;
    let input = memory_range_bytes(ecx, code_range);
    let create_cost = if is_create2 {
        version.gas_params.create2_cost(len)
    } else {
        version.gas_params.get(GasId::Create).into()
    };
    spend(&mut ecx.gas, create_cost)?;
    let gas_limit = if ecx.spec_id.enables(SpecId::TANGERINE) {
        version.gas_params.call_stipend_reduction(ecx.gas.remaining())
    } else {
        ecx.gas.remaining()
    };
    spend(&mut ecx.gas, gas_limit)?;

    let current = ecx.message;
    let mut message = Message {
        kind: if is_create2 { MessageKind::Create2 } else { MessageKind::Create },
        depth: current.depth.saturating_add(1),
        gas_limit,
        destination: current.destination,
        caller: current.destination,
        input,
        value,
        code_address: current.destination,
        disable_precompiles: false,
        salt: salt.map(|salt| alloy_primitives::B256::from(salt.to_be_bytes())).unwrap_or_default(),
        ext: (),
        _non_exhaustive: (),
    };
    let bytecode = Bytecode::new_legacy(message.input.clone());
    let result = ecx.host.execute_message(ecx.tx_env, bytecode, &mut message, false);
    ecx.gas.erase_cost(result.gas_returned_to_parent());
    ecx.gas.record_refund(result.refund_propagated_to_parent());

    let return_data = if result.stop == InstrStop::Revert { result.output } else { Bytes::new() };
    let address = result
        .created_address
        .filter(|_| result.stop.is_success())
        .map(|address| EvmWord::from_be_slice(address.as_slice()))
        .unwrap_or_default();
    unsafe {
        sp.write(address);
    }
    ecx.set_return_data(return_data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Bytes as AlloyBytes, Log};
    use core::mem::offset_of;
    use evm2::{
        BaseEvmConfigSelector, Evm, EvmConfigSelector, Precompiles,
        bytecode::Bytecode,
        env::{BlockEnv, TxEnv},
        ethereum::ethereum_tx_registry,
        evm::{AccountLoad, EmptyDB, SLoad, SStore, SelfDestructResult},
        interpreter::{GasTracker, Host, Message, MessageKind, MessageResult, op},
    };

    #[derive(Debug)]
    struct TestHost {
        block: BlockEnv<BaseEvmTypes>,
        code: AlloyBytes,
        execute_result: MessageResult<BaseEvmTypes>,
        calls: Vec<Message<BaseEvmTypes>>,
        call_static_flags: Vec<bool>,
    }

    impl Default for TestHost {
        fn default() -> Self {
            Self {
                block: BlockEnv::default(),
                code: AlloyBytes::new(),
                execute_result: MessageResult {
                    stop: InstrStop::Return,
                    ..MessageResult::default()
                },
                calls: Vec::new(),
                call_static_flags: Vec::new(),
            }
        }
    }

    impl Host<BaseEvmTypes> for TestHost {
        fn spec_id(&self) -> SpecId {
            SpecId::CANCUN
        }

        fn block_env(&mut self) -> &BlockEnv<BaseEvmTypes> {
            &self.block
        }

        fn load_account(
            &mut self,
            address: &Address,
            load_code: bool,
            _skip_cold_load: bool,
        ) -> Result<AccountLoad, InstrStop> {
            Ok(AccountLoad {
                balance: address.into_word().into(),
                code_hash: B256::ZERO,
                code: if load_code {
                    Bytecode::new_legacy(self.code.clone())
                } else {
                    Bytecode::default()
                },
                exists: true,
                is_empty: false,
                is_cold: false,
                _non_exhaustive: (),
            })
        }

        fn target_is_empty_for_new_account_gas(
            &mut self,
            _address: &Address,
            _spec_id: SpecId,
        ) -> Result<bool, InstrStop> {
            Ok(false)
        }

        fn block_hash(&mut self, number: &Word) -> Result<Option<B256>, InstrStop> {
            Ok(Some(B256::with_last_byte(number.wrapping_to::<u8>())))
        }

        fn sload(
            &mut self,
            _address: &Address,
            _key: &Word,
            _skip_cold_load: bool,
        ) -> Result<SLoad, InstrStop> {
            Ok(SLoad { value: Word::ZERO, is_cold: false, _non_exhaustive: () })
        }

        fn sstore(
            &mut self,
            _address: &Address,
            _key: &Word,
            value: &Word,
            _skip_cold_load: bool,
        ) -> Result<SStore, InstrStop> {
            Ok(SStore {
                original_value: Word::ZERO,
                present_value: Word::ZERO,
                new_value: *value,
                is_cold: false,
                _non_exhaustive: (),
            })
        }

        fn tload(&mut self, _address: &Address, _key: &Word) -> Word {
            Word::ZERO
        }

        fn tstore(&mut self, _address: &Address, _key: &Word, _value: &Word) {}

        fn log(&mut self, _log: Log) {}

        fn execute_message(
            &mut self,
            _tx_env: &TxEnv<BaseEvmTypes>,
            _bytecode: Bytecode,
            message: &mut Message<BaseEvmTypes>,
            caller_is_static: bool,
        ) -> MessageResult<BaseEvmTypes> {
            self.call_static_flags
                .push(caller_is_static || message.kind == MessageKind::StaticCall);
            self.calls.push(message.clone());
            self.execute_result.clone()
        }

        fn selfdestruct(
            &mut self,
            _contract: &Address,
            _target: &Address,
            _skip_cold_load: bool,
        ) -> Result<SelfDestructResult, InstrStop> {
            Ok(SelfDestructResult::default())
        }
    }

    struct PreparedJitFrame<'a> {
        ecx: EvmContext<'a>,
        stack: &'a mut EvmStack,
    }

    fn address_word(address: &Address) -> EvmWord {
        EvmWord::from_be_slice(address.as_slice())
    }

    fn write_call_stack(
        stack: &mut EvmStack,
        target: Address,
        gas_limit: u64,
        value: Option<Word>,
    ) {
        stack.set(0, EvmWord::ZERO);
        stack.set(1, EvmWord::ZERO);
        stack.set(2, EvmWord::ZERO);
        stack.set(3, EvmWord::ZERO);
        if let Some(value) = value {
            stack.set(4, EvmWord::from(value));
            stack.set(5, address_word(&target));
            stack.set(6, EvmWord::from(Word::from(gas_limit)));
        } else {
            stack.set(4, address_word(&target));
            stack.set(5, EvmWord::from(Word::from(gas_limit)));
        }
    }

    fn base_evm(spec_id: SpecId) -> Evm<BaseEvmTypes> {
        Evm::<BaseEvmTypes>::new(
            spec_id,
            BlockEnv::default(),
            ethereum_tx_registry(spec_id),
            EmptyDB::default(),
            Precompiles::base(spec_id),
        )
    }

    fn prepare_frame<'a>(
        interpreter: &'a mut Interpreter<'_, BaseEvmTypes>,
        host: &'a mut TestHost,
    ) -> PreparedJitFrame<'a> {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let mut prepare_host = base_evm(SpecId::CANCUN);
        interpreter.prepare_jit_run(&config, &mut prepare_host);
        let (ecx, stack, _stack_len) = EvmContext::from_interpreter_with_stack(interpreter, host);
        PreparedJitFrame { ecx, stack }
    }

    fn base_context<'a, 'ctx>(ecx: &'a mut EvmContext<'ctx>) -> &'a mut crate::EvmContext<'ctx> {
        unsafe { &mut *ptr::from_mut(ecx).cast::<crate::EvmContext<'_>>() }
    }

    #[test]
    fn evm2_context_matches_imported_context_offsets() {
        assert_eq!(offset_of!(EvmContext<'_>, input), offset_of!(crate::EvmContext<'_>, input));
        assert_eq!(offset_of!(EvmContext<'_>, gas), offset_of!(crate::EvmContext<'_>, gas));
        assert_eq!(offset_of!(EvmContext<'_>, spec_id), offset_of!(crate::EvmContext<'_>, spec_id));
        assert_eq!(
            offset_of!(EvmContext<'_>, mem_base),
            offset_of!(crate::EvmContext<'_>, mem_base)
        );
        assert_eq!(offset_of!(EvmContext<'_>, mem_len), offset_of!(crate::EvmContext<'_>, mem_len));
    }

    #[test]
    fn evm2_gas_is_used_directly() {
        let mut gas = Evm2Gas::new_with_regular_gas_and_reservoir(100, 20);
        gas.set_remaining(77);
        gas.set_state_gas_spent(11);
        gas.set_refunded(3);
        gas.memory_mut().words_num = 4;
        gas.memory_mut().expansion_cost = 12;

        assert_eq!(gas.limit(), 100);
        assert_eq!(gas.remaining(), 77);
        assert_eq!(gas.reservoir(), 20);
        assert_eq!(gas.state_gas_spent(), 11);
        assert_eq!(gas.refunded(), 3);
        assert_eq!(gas.memory().words_num, 4);
        assert_eq!(gas.memory().expansion_cost, 12);
    }

    #[test]
    fn evm2_context_host_context_uses_evm2_env() {
        let tx_origin = Address::from([0x11; 20]);
        let beneficiary = Address::from([0x22; 20]);
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv { origin: tx_origin, gas_price: Word::from(7), ..TxEnv::default() };
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv { number: Word::from(9), beneficiary, ..BlockEnv::default() },
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        interpreter.prepare_jit_run(&config, &mut host);
        let ecx = EvmContext::from_interpreter(&mut interpreter, &mut host);

        assert_eq!(ecx.host.caller(), tx_origin);
        assert_eq!(ecx.host.effective_gas_price(), Word::from(7));
        assert_eq!(ecx.host.block_number(), Word::from(9));
        assert_eq!(ecx.host.beneficiary(), beneficiary);
    }

    #[test]
    fn evm2_context_uses_memory_scratch() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        interpreter.prepare_jit_run(&config, &mut host);
        {
            let memory = interpreter.memory_mut();
            memory.resize(0, 3).unwrap();
            memory.set(0, b"abc");
        }

        let ecx = EvmContext::from_interpreter(&mut interpreter, &mut host);
        assert_eq!(unsafe { &*ecx.memory }.as_slice(), b"abc");
        unsafe { &mut *ecx.memory }.set(1, b"z");
        let state = ecx.interpreter_state();
        drop(ecx);
        state.store(&mut interpreter);

        assert_eq!(interpreter.memory_ref().slice(0, 3), b"azc");
    }

    #[test]
    fn execute_call_message_executes_message() {
        let target = Address::from([0x22; 20]);
        let caller = Address::from([0x11; 20]);
        let child_output = AlloyBytes::from_static(&[0xaa, 0xbb, 0xcc]);
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(37),
                output: child_output.clone(),
                ..MessageResult::default()
            },
            ..TestHost::default()
        };
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, destination: caller, ..Message::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            unsafe { &mut *frame.ecx.memory }.resize(0, 16).unwrap();
            unsafe { &mut *frame.ecx.memory }.set(4, b"in");
            frame.stack.set(0, EvmWord::from(Word::from(2)));
            frame.stack.set(1, EvmWord::from(Word::from(8)));
            frame.stack.set(2, EvmWord::from(Word::from(2)));
            frame.stack.set(3, EvmWord::from(Word::from(4)));
            frame.stack.set(4, EvmWord::ZERO);
            frame.stack.set(5, address_word(&target));
            frame.stack.set(6, EvmWord::from(Word::from(50_000)));

            let sp = frame.stack.as_mut_ptr();
            let result = unsafe { execute_call_message(base_context(&mut frame.ecx), sp, 0) };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
            assert_eq!(frame.ecx.return_data, child_output.as_ref());
            assert_eq!(unsafe { &*frame.ecx.memory }.slice(8, 2), &[0xaa, 0xbb]);
            frame.ecx.output = AlloyBytes::copy_from_slice(b"frame-output");
            assert_eq!(frame.ecx.return_data, child_output.as_ref());
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert_eq!(host.calls[0].destination, target);
        assert_eq!(host.calls[0].caller, caller);
        assert_eq!(host.calls[0].input.as_ref(), b"in");
        assert!(!host.call_static_flags[0]);
    }

    #[test]
    fn execute_call_message_callcode_maps_message_fields() {
        let target = Address::from([0x22; 20]);
        let destination = Address::from([0x33; 20]);
        let caller = Address::from([0x11; 20]);
        let stack_value = Word::from(0x12);
        let mut host = TestHost::default();
        let tx_env = TxEnv::default();
        let message = Message {
            gas_limit: 1_000_000,
            destination,
            caller,
            value: Word::from(0x99),
            ..Message::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            write_call_stack(frame.stack, target, 50_000, Some(stack_value));

            let result = unsafe {
                execute_call_message(base_context(&mut frame.ecx), frame.stack.as_mut_ptr(), 1)
            };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::CallCode);
        assert_eq!(host.calls[0].destination, destination);
        assert_eq!(host.calls[0].caller, destination);
        assert_eq!(host.calls[0].value, stack_value);
        assert_eq!(host.calls[0].code_address, target);
        assert!(!host.call_static_flags[0]);
    }

    #[test]
    fn execute_call_message_delegatecall_maps_message_fields() {
        let target = Address::from([0x22; 20]);
        let destination = Address::from([0x33; 20]);
        let caller = Address::from([0x11; 20]);
        let current_value = Word::from(0x99);
        let mut host = TestHost::default();
        let tx_env = TxEnv::default();
        let message = Message {
            gas_limit: 1_000_000,
            destination,
            caller,
            value: current_value,
            ..Message::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            write_call_stack(frame.stack, target, 50_000, None);

            let result = unsafe {
                execute_call_message(base_context(&mut frame.ecx), frame.stack.as_mut_ptr(), 2)
            };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::DelegateCall);
        assert_eq!(host.calls[0].destination, destination);
        assert_eq!(host.calls[0].caller, caller);
        assert_eq!(host.calls[0].value, current_value);
        assert_eq!(host.calls[0].code_address, target);
        assert!(!host.call_static_flags[0]);
    }

    #[test]
    fn execute_call_message_staticcall_maps_message_fields() {
        let target = Address::from([0x22; 20]);
        let destination = Address::from([0x33; 20]);
        let caller = Address::from([0x11; 20]);
        let mut host = TestHost::default();
        let tx_env = TxEnv::default();
        let message = Message {
            gas_limit: 1_000_000,
            destination,
            caller,
            value: Word::from(0x99),
            ..Message::default()
        };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            write_call_stack(frame.stack, target, 50_000, None);

            let result = unsafe {
                execute_call_message(base_context(&mut frame.ecx), frame.stack.as_mut_ptr(), 3)
            };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }.to_u256(), Word::from(1));
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::StaticCall);
        assert_eq!(host.calls[0].destination, target);
        assert_eq!(host.calls[0].caller, destination);
        assert_eq!(host.calls[0].value, Word::ZERO);
        assert_eq!(host.calls[0].code_address, target);
        assert!(host.call_static_flags[0]);
    }

    #[test]
    fn execute_create_message_executes_message() {
        let created = Address::from([0x77; 20]);
        let initcode = [op::STOP];
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(11),
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..TestHost::default()
        };
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Message::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            unsafe { &mut *frame.ecx.memory }.resize(0, 1).unwrap();
            unsafe { &mut *frame.ecx.memory }.set(0, &initcode);
            frame.stack.set(0, EvmWord::from(Word::from(initcode.len())));
            frame.stack.set(1, EvmWord::ZERO);
            frame.stack.set(2, EvmWord::ZERO);

            let sp = frame.stack.as_mut_ptr();
            let result = unsafe { execute_create_message(base_context(&mut frame.ecx), sp, 0) };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }, &address_word(&created));
            assert!(frame.ecx.return_data.is_empty());
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Create);
        assert_eq!(host.calls[0].input.as_ref(), initcode);
    }

    #[test]
    fn execute_create_message_create2_maps_salt() {
        let created = Address::from([0x77; 20]);
        let initcode = [op::STOP];
        let salt = EvmWord::from(Word::from(0xabcdu64));
        let mut host = TestHost {
            execute_result: MessageResult {
                stop: InstrStop::Return,
                gas: GasTracker::new(11),
                created_address: Some(created),
                ..MessageResult::default()
            },
            ..TestHost::default()
        };
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Message::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(AlloyBytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );

        {
            let mut frame = prepare_frame(&mut interpreter, &mut host);
            unsafe { &mut *frame.ecx.memory }.resize(0, 1).unwrap();
            unsafe { &mut *frame.ecx.memory }.set(0, &initcode);
            frame.stack.set(0, salt);
            frame.stack.set(1, EvmWord::from(Word::from(initcode.len())));
            frame.stack.set(2, EvmWord::ZERO);
            frame.stack.set(3, EvmWord::ZERO);

            let result = unsafe {
                execute_create_message(base_context(&mut frame.ecx), frame.stack.as_mut_ptr(), 1)
            };

            assert_eq!(result, Ok(()));
            assert_eq!(unsafe { frame.stack.get_unchecked(0) }, &address_word(&created));
            assert!(frame.ecx.return_data.is_empty());
        }

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Create2);
        assert_eq!(host.calls[0].input.as_ref(), initcode);
        assert_eq!(host.calls[0].salt, B256::from(salt.to_be_bytes()));
    }

    unsafe extern "C" fn evm2_return_output(
        mut ecx: NonNull<crate::EvmContext<'_>>,
        _stack: NonNull<EvmStack>,
        _stack_len: NonNull<usize>,
    ) -> InstrStop {
        let ecx = unsafe { ecx.as_mut() };
        ecx.output = AlloyBytes::copy_from_slice(b"ok");
        InstrStop::Return
    }

    #[test]
    fn evm2_call_with_interpreter_maps_return_output() {
        let config = <BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::execution_config(
            SpecId::CANCUN,
        );
        let tx_env = TxEnv::default();
        let message = Message { gas_limit: 1_000_000, ..Default::default() };
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(
            Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[op::STOP])),
            &tx_env,
            &message,
            false,
        );
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnv::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            EmptyDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );

        interpreter.prepare_jit_run(&config, &mut host);
        let stop = unsafe {
            EvmCompilerFn::new(evm2_return_output)
                .call_with_interpreter(&mut interpreter, &mut host)
        };

        assert_eq!(stop, InstrStop::Return);
        assert_eq!(interpreter.output(), b"ok");
    }
}
