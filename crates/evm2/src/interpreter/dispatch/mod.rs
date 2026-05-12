//! Instruction dispatch tables.

#[cfg(not(tco))]
use crate::interpreter::Gas;
#[cfg(tco)]
use crate::interpreter::gas::RemainingGas;
use crate::{
    BaseEvmConfigSelector, EvmConfig, EvmConfigSelector, EvmTypes, VersionTables,
    evm::config::SelectorVersionTables,
    interpreter::{InstrStop, Interpreter, InterpreterState, Pc, Result, Stack, StackMut, op},
    trustme,
};
#[cfg(not(tco))]
use core::hint::cold_path;

#[cold]
pub(crate) const fn unknown_instruction<T: EvmTypes>(
    _pc: &mut Pc,
    _stack: StackMut<'_>,
    _state: &mut InterpreterState<'_, T>,
) -> Result {
    Err(InstrStop::OpcodeNotFound)
}

macro_rules! for_each_opcode_value {
    ([$($extra:tt)*] $m:ident) => {
        $m! { [$($extra)*]
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
            0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
            0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
            0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
            0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
            0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
            0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57,
            0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67,
            0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
            0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77,
            0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
            0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
            0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
            0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
            0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
            0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7,
            0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
            0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7,
            0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
            0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7,
            0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
            0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
            0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
            0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7,
            0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
        }
    };
}

#[cfg(not(tco))]
macro_rules! assign_instruction_table_entries {
    ([$table:expr, $evm_types:ty, $config:ty, $mode:ty, $vt:ident, $previous_vt:ident, $dispatch:ident, $instr_fn:ty] $($op:literal,)*) => {
        $(
            if super::instruction_changed($vt, $previous_vt, $op) {
                let instruction = <$config as crate::EvmConfig<$evm_types>>::VERSION_TABLES.instruction($op);
                $table[$op] = if instruction.dynamic_gas {
                    $dispatch::<$evm_types, $config, $mode, $op, true> as $instr_fn
                } else {
                    $dispatch::<$evm_types, $config, $mode, $op, false> as $instr_fn
                };
            }
        )*
    };
}

#[cfg(not(tco))]
macro_rules! make_selector_tables {
    ([$($extra:tt)*] $($spec:ident $name:ident,)*) => {{
        make_selector_tables!(@build [] [none]; $($spec $name,)*)
    }};
    (@build [$($tables:ident,)*] [$($previous_table:tt)*]; $spec:ident $name:ident, $($rest:ident $rest_name:ident,)*) => {{
        let spec = crate::SpecId::$spec;
        let previous = spec.prev();
        let $name = make_table::<T, F::Config<{ crate::SpecId::$spec as u8 }, CUSTOM_SPEC_ID>, M>(
            make_selector_tables!(@previous_table [$($previous_table)*]),
            match previous {
                Some(previous) => {
                    Some(crate::evm::config::SelectorVersionTables::<T, F, CUSTOM_SPEC_ID>::VERSION_TABLES[previous as usize])
                }
                None => None,
            },
        );
        make_selector_tables!(@build [$($tables,)* $name,] [some $name]; $($rest $rest_name,)*)
    }};
    (@build [$($tables:ident,)*] [$($previous_table:tt)*];) => {
        [$($tables,)*]
    };
    (@previous_table [none]) => {
        None
    };
    (@previous_table [some $previous_table:ident]) => {
        Some(&$previous_table)
    };
}

#[cfg(not(tco))]
macro_rules! dispatch_tables {
    () => {
        /// Instruction dispatch table.
        pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

        pub(super) const fn make_table<T, C, M>(
            previous: Option<&RawInstrTable<T>>,
            previous_version_tables: Option<&crate::VersionTables<T>>,
        ) -> RawInstrTable<T>
        where
            T: crate::EvmTypes,
            C: crate::EvmConfig<T>,
            M: super::InspectMode<T>,
        {
            let mut table = match previous {
                Some(previous) => *previous,
                None => [dispatch::<T, C, M, 0, true> as super::InstrFn<T>; 256],
            };
            let vt = C::VERSION_TABLES;
            for_each_opcode_value!([table, T, C, M, vt, previous_version_tables, dispatch, super::InstrFn<T>] assign_instruction_table_entries);

            // Make all unknown entries point to the same dispatch function.
            let mut i = 0;
            let mut unknown_idx = None;
            while i < 256 {
                if C::VERSION_TABLES.is_unknown_opcode(i as u8) {
                    if unknown_idx.is_none() {
                        unknown_idx = Some(i);
                    }
                    table[i] = table[unknown_idx.unwrap()];
                }
                i += 1;
            }

            table
        }

        pub(super) const fn make_selector_tables<
            T,
            F,
            M,
            const CUSTOM_SPEC_ID: u8,
        >() -> [RawInstrTable<T>; crate::SpecId::COUNT]
        where
            T: crate::EvmTypes,
            F: crate::EvmConfigSelector<T>,
            M: super::InspectMode<T>,
        {
            crate::for_each_spec!([] make_selector_tables)
        }
    };
}

cfg_if::cfg_if! {
    if #[cfg(tco)] {
        mod tco;
        use tco as imp;
    } else if #[cfg(dispatch_packed)] {
        mod packed;
        use packed as imp;
    } else if #[cfg(dispatch_single_return)] {
        mod single_return;
        use single_return as imp;
    } else {
        mod unpacked;
        use unpacked as imp;
    }
}

/// Instruction function pointer.
type InstrFn<T> = imp::RawInstrFn<T>;

/// Instruction dispatch table.
pub(crate) type InstrTable<T> = imp::RawInstrTable<T>;

#[cfg(not(tco))]
pub(in crate::interpreter) fn run_table_loop<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &InstrTable<T>,
) -> InstrStop {
    // SAFETY: Only the active interpreter lifetime is erased; this stays as a raw pointer so
    // the dispatch loop does not create an extra `&mut` alias for `interpreter`.
    let raw = unsafe { trustme::decouple_lt_mut_ptr(interpreter as *mut Interpreter<'_, T>) };
    // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
    // the separate stack view is live.
    let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
    let mut pc = Pc::new(interpreter.pc);
    let mut stack = Stack::new(&mut interpreter.stack, interpreter.stack_len);
    let mut loop_state = loop_state(&interpreter.gas);
    loop {
        let op = pc.op();
        let instr = instructions[op as usize];
        let (next_pc, next_stack_len) =
            dispatch_loop_call(instr, pc, stack.reborrow(), state, &mut loop_state);
        pc = next_pc;
        stack.len = next_stack_len;

        if pc.as_ptr().is_null() {
            cold_path();
            interpreter.pc = pc.as_ptr();
            interpreter.stack_len = stack.len;
            finish_loop(&mut interpreter.gas, loop_state);
            return interpreter.result.unwrap_err();
        }
    }
}

#[inline(always)]
#[cfg(tco)]
pub(in crate::interpreter) fn run_tail<T: EvmTypes>(
    interpreter: &mut Interpreter<'_, T>,
    instructions: &InstrTable<T>,
) -> InstrStop {
    // SAFETY: Only the active interpreter lifetime is erased; this stays as a raw pointer so
    // the dispatch step does not create an extra `&mut` alias for `interpreter`.
    let raw = unsafe { trustme::decouple_lt_mut_ptr(interpreter as *mut Interpreter<'_, T>) };
    // SAFETY: Instruction methods must not access the stack through `InterpreterState` while
    // the separate stack view is live.
    let state = InterpreterState::wrap_mut(unsafe { &mut *raw });
    let pc = Pc::new(interpreter.pc);
    let op = pc.op();
    let stack = Stack::new(&mut interpreter.stack, interpreter.stack_len);
    let remaining_gas = RemainingGas::new(interpreter.gas.remaining());
    let instr = instructions[op as usize];
    instr(pc, stack, remaining_gas, state, (instructions as *const InstrTable<T>).cast());
    interpreter.result.unwrap_err()
}

#[cfg(not(tco))]
type LoopState = imp::LoopState;

#[cfg(not(tco))]
#[inline(always)]
const fn loop_state(gas: &Gas) -> LoopState {
    imp::loop_state(gas)
}

#[cfg(not(tco))]
#[inline(always)]
fn dispatch_loop_call<T: EvmTypes>(
    instr: InstrFn<T>,
    pc: Pc,
    stack: Stack<'_>,
    state: &mut InterpreterState<'_, T>,
    loop_state: &mut LoopState,
) -> (Pc, usize) {
    imp::dispatch_loop_call(instr, pc, stack, state, loop_state)
}

#[cfg(not(tco))]
#[inline(always)]
const fn finish_loop(gas: &mut Gas, loop_state: LoopState) {
    imp::finish_loop(gas, loop_state);
}

pub(crate) struct ConfigInstrTables<T, C>(core::marker::PhantomData<fn() -> (T, C)>);

impl<T, C> ConfigInstrTables<T, C>
where
    T: EvmTypes,
    C: EvmConfig<T>,
{
    pub(crate) const INSTRUCTIONS: &'static InstrTable<T> = &imp::make_table::<T, C, NoInspector>(
        Some(
            &SelectorInstrTables::<T, BaseEvmConfigSelector, { u8::MAX }>::INSTRUCTIONS
                [C::BASE_SPEC_ID as usize],
        ),
        Some(
            SelectorVersionTables::<T, BaseEvmConfigSelector, { u8::MAX }>::VERSION_TABLES
                [C::BASE_SPEC_ID as usize],
        ),
    );
    pub(crate) const INSPECT_INSTRUCTIONS: &'static InstrTable<T> =
        &imp::make_table::<T, C, DynInspector>(
            Some(
                &SelectorInstrTables::<T, BaseEvmConfigSelector, { u8::MAX }>::INSPECT_INSTRUCTIONS
                    [C::BASE_SPEC_ID as usize],
            ),
            Some(
                SelectorVersionTables::<T, BaseEvmConfigSelector, { u8::MAX }>::VERSION_TABLES
                    [C::BASE_SPEC_ID as usize],
            ),
        );
}

pub(crate) struct SelectorInstrTables<T, F, const CUSTOM_SPEC_ID: u8>(
    core::marker::PhantomData<fn() -> (T, F)>,
);

impl<T, F, const CUSTOM_SPEC_ID: u8> SelectorInstrTables<T, F, CUSTOM_SPEC_ID>
where
    T: EvmTypes,
    F: EvmConfigSelector<T>,
{
    pub(crate) const INSTRUCTIONS: &'static [InstrTable<T>; crate::SpecId::COUNT] =
        &imp::make_selector_tables::<T, F, NoInspector, CUSTOM_SPEC_ID>();
    pub(crate) const INSPECT_INSTRUCTIONS: &'static [InstrTable<T>; crate::SpecId::COUNT] =
        &imp::make_selector_tables::<T, F, DynInspector, CUSTOM_SPEC_ID>();
}

const fn instruction_changed<T: EvmTypes>(
    version_tables: &VersionTables<T>,
    previous_version_tables: Option<&VersionTables<T>>,
    op: u8,
) -> bool {
    let Some(previous_version_tables) = previous_version_tables else {
        return true;
    };
    version_tables.revision(op) != previous_version_tables.revision(op)
}

#[inline(always)]
const fn inc_pc(pc: &mut Pc, op: u8) {
    unsafe { pc.advance_unchecked(instruction_len(op)) };
}

#[inline(always)]
const fn instruction_len(op: u8) -> usize {
    match op {
        op::JUMP | op::JUMPI => 0, // Set inside.
        op::PUSH1..=op::PUSH32 => (op - op::PUSH1 + 2) as usize,
        op::DUPN | op::SWAPN | op::EXCHANGE => 2,
        _ => 1,
    }
}

trait InspectMode<T: EvmTypes> {
    const INSPECT: bool;

    fn step(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize);

    fn step_end(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize);
}

struct NoInspector;

impl<T: EvmTypes> InspectMode<T> for NoInspector {
    const INSPECT: bool = false;

    #[inline(always)]
    fn step(_state: &mut InterpreterState<'_, T>, _pc: Pc, _stack_len: usize) {}

    #[inline(always)]
    fn step_end(_state: &mut InterpreterState<'_, T>, _pc: Pc, _stack_len: usize) {}
}

struct DynInspector;

impl<T: EvmTypes> InspectMode<T> for DynInspector {
    const INSPECT: bool = true;

    #[inline(always)]
    fn step(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step(pc, stack_len);
    }

    #[inline(always)]
    fn step_end(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize) {
        state.inspect_step_end(pc, stack_len);
    }
}

#[cfg(test)]
mod tests {
    use crate::{BaseEvmConfig, BaseEvmTypes, EvmConfig, SpecId, VersionTables, interpreter::op};

    fn version_tables(spec: SpecId) -> &'static VersionTables<BaseEvmTypes> {
        crate::spec_to_generic!(spec, |BASE_SPEC_ID| {
            <BaseEvmConfig<BASE_SPEC_ID> as EvmConfig<BaseEvmTypes>>::VERSION_TABLES
        })
    }

    #[test]
    fn default_gas_table_matches_revm_static_costs() {
        let default_gas_table = version_tables(SpecId::FRONTIER);
        assert_eq!(default_gas_table.static_gas(op::STOP), 0);
        assert_eq!(default_gas_table.static_gas(op::ADD), 3);
        assert_eq!(default_gas_table.static_gas(op::MUL), 5);
        assert_eq!(default_gas_table.static_gas(op::EXP), 10);
        assert_eq!(default_gas_table.static_gas(op::BALANCE), 20);
        assert_eq!(default_gas_table.static_gas(op::SLOAD), 50);
        assert_eq!(default_gas_table.static_gas(op::CALL), 40);
        assert_eq!(default_gas_table.static_gas(op::SELFDESTRUCT), 0);
    }

    #[test]
    fn gas_table_applies_spec_static_costs() {
        let tangerine = version_tables(SpecId::TANGERINE);
        assert_eq!(tangerine.static_gas(op::SLOAD), 200);
        assert_eq!(tangerine.static_gas(op::BALANCE), 400);
        assert_eq!(tangerine.static_gas(op::SELFDESTRUCT), 5000);

        let istanbul = version_tables(SpecId::ISTANBUL);
        assert_eq!(istanbul.static_gas(op::SLOAD), 800);
        assert_eq!(istanbul.static_gas(op::EXTCODEHASH), 700);

        let berlin = version_tables(SpecId::BERLIN);
        assert_eq!(berlin.static_gas(op::SLOAD), 100);
        assert_eq!(berlin.static_gas(op::BALANCE), 100);
        assert_eq!(berlin.static_gas(op::CALL), 100);
    }
}
