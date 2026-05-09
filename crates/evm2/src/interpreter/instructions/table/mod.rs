//! Instruction dispatch tables.

use crate::{
    EvmConfigSelector, EvmTypes, InstrTables, VersionTables,
    interpreter::{InstrStop, InterpreterState, Pc, Result, StackMut, op},
};

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

cfg_if::cfg_if! {
    if #[cfg(tco)] {
        mod tco;
        use tco as imp;
    } else {
        mod normal;
        use normal as imp;
    }
}

/// Instruction function pointer.
pub(crate) type InstrFn<T> = imp::RawInstrFn<T>;

/// Instruction dispatch table.
pub(crate) type InstrTable<T> = imp::RawInstrTable<T>;

#[allow(private_interfaces)]
impl<T, F> InstrTables<F> for T
where
    T: EvmTypes,
    F: EvmConfigSelector<T>,
{
    const INSTRUCTIONS: &'static [InstrTable<Self>; crate::SpecId::COUNT] =
        &imp::make_selector_tables::<Self, F, NoInspector>();
    const INSPECT_INSTRUCTIONS: &'static [InstrTable<Self>; crate::SpecId::COUNT] =
        &imp::make_selector_tables::<Self, F, DynInspector>();
}

pub(super) const fn instruction_changed<T: EvmTypes>(
    version_tables: &VersionTables<T>,
    previous_version_tables: Option<&VersionTables<T>>,
    op: u8,
) -> bool {
    let Some(previous_version_tables) = previous_version_tables else {
        return true;
    };
    let instruction = version_tables.instruction(op);
    let previous_instruction = previous_version_tables.instruction(op);
    version_tables.static_gas_revision(op) != previous_version_tables.static_gas_revision(op)
        || instruction.revision != previous_instruction.revision
}

#[inline(always)]
pub(super) const fn inc_pc(pc: &mut Pc, op: u8) {
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

pub(crate) trait InspectMode<T: EvmTypes> {
    const INSPECT: bool;

    fn step(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize);

    fn step_end(state: &mut InterpreterState<'_, T>, pc: Pc, stack_len: usize);
}

pub(crate) struct NoInspector;

impl<T: EvmTypes> InspectMode<T> for NoInspector {
    const INSPECT: bool = false;

    #[inline(always)]
    fn step(_state: &mut InterpreterState<'_, T>, _pc: Pc, _stack_len: usize) {}

    #[inline(always)]
    fn step_end(_state: &mut InterpreterState<'_, T>, _pc: Pc, _stack_len: usize) {}
}

pub(crate) struct DynInspector;

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
