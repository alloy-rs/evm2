//! Sections whose static gas does not fit in 32 bits.

use super::{DEF_SPEC, with_evm_context};
use crate::{Backend, EvmCompiler, parse_asm};
use alloy_primitives::U256;
use evm2::{
    Version,
    interpreter::{Gas, InstrStop},
    version::GasId,
};

/// `ExpByte` for the test, with a 32-byte exponent: `EXP`'s dynamic gas alone exceeds `u32::MAX`.
///
/// The rest of the section is `PUSH32` (3), `CALLVALUE` (2) and `EXP`'s base gas (10).
const EXP_BYTE: u32 = u32::MAX;
const EXP_GAS: u64 = EXP_BYTE as u64 * 32;
const SPENT: u64 = 3 + 2 + 10 + EXP_GAS;

/// The exponent, 32 bytes long.
const MAX: &str = "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// Gas limit of the test frame.
const LIMIT: u64 = 2 * SPENT;

/// Runs `src` and returns the final stack, checking the gas spent.
fn run<B: Backend>(compiler: &mut EvmCompiler<B>, src: &str, expected: u64) -> Vec<U256> {
    unsafe { compiler.clear() }.unwrap();
    let mut gas_params = Version::base(DEF_SPEC).gas_params;
    gas_params.set(GasId::ExpByte, EXP_BYTE);
    compiler.set_gas_params(gas_params);
    compiler.set_inspect_stack(true);
    let code = parse_asm(src).unwrap();
    let f = unsafe { compiler.jit("large_section_gas", &code, DEF_SPEC) }.unwrap();
    with_evm_context(&code, DEF_SPEC, |ecx, stack, stack_len| {
        ecx.gas = Gas::new(LIMIT);
        let r = unsafe { f.call(ecx, stack, stack_len) };
        assert_eq!(r, InstrStop::Stop);
        assert_eq!(ecx.gas.spent(), expected);
        unsafe { stack.as_slice(*stack_len) }.iter().map(|word| word.to_u256()).collect()
    })
}

// The section cost exceeds `u32::MAX`, and a constant exponent above 2 is charged with the
// section.
matrix_tests!(
    large_section_gas_const_exponent = |compiler| {
        run(compiler, &format!("PUSH32 {MAX} CALLVALUE EXP STOP"), SPENT);
    }
);

// The section head charges the whole cost, so the `GAS` after it owes the part past `u32::MAX`
// back to the remaining gas it reads.
matrix_tests!(
    large_section_gas_owed = |compiler| {
        let stack = run(compiler, &format!("GAS PUSH32 {MAX} CALLVALUE EXP GAS STOP"), SPENT + 4);
        assert_eq!(stack[0], U256::from(LIMIT - 2));
        assert_eq!(stack[2], U256::from(LIMIT - SPENT - 4));
    }
);
