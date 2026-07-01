#![no_main]

use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, RevmBackend, SpecId, bytecode_case_with_spec,
    compare_case,
};
use libfuzzer_sys::fuzz_target;

const STOP: u8 = 0x00;
const POP: u8 = 0x50;
const CALL: u8 = 0xf1;
const CALLCODE: u8 = 0xf2;
const DELEGATECALL: u8 = 0xf4;
const STATICCALL: u8 = 0xfa;
const SELFDESTRUCT: u8 = 0xff;

const NON_AMSTERDAM_SPECS: &[SpecId] = &[
    SpecId::FRONTIER,
    SpecId::HOMESTEAD,
    SpecId::TANGERINE,
    SpecId::SPURIOUS_DRAGON,
    SpecId::BYZANTIUM,
    SpecId::PETERSBURG,
    SpecId::ISTANBUL,
    SpecId::BERLIN,
    SpecId::LONDON,
    SpecId::MERGE,
    SpecId::SHANGHAI,
    SpecId::CANCUN,
    SpecId::PRAGUE,
    SpecId::OSAKA,
];

const ADDRESSES: &[[u8; 20]] =
    &[[0x00; 20], [0x10; 20], [0x20; 20], [0x30; 20], [0x40; 20], [0xff; 20]];

const GAS_LIMITS: &[u64] = &[0, 1, 2_299, 2_300, 2_301, 5_000, 25_000, 100_000, 1_000_000];
const VALUES: &[u64] = &[0, 1, 2, 1_000_000];

fuzz_target!(|data: &[u8]| {
    let Some((&spec_byte, ops)) = data.split_first() else {
        return;
    };

    let spec = NON_AMSTERDAM_SPECS[usize::from(spec_byte) % NON_AMSTERDAM_SPECS.len()];
    let bytecode = lifecycle_program(ops, spec);
    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) =
        compare_case(&backends, &bytecode_case_with_spec(spec, &bytecode), CaseContext::Bytes)
    {
        panic!("{err}");
    }
});

fn lifecycle_program(data: &[u8], spec: SpecId) -> Vec<u8> {
    let mut code = Vec::new();
    for chunk in data.chunks(4).take(16) {
        let selector = chunk.first().copied().unwrap_or_default();
        let address =
            ADDRESSES[usize::from(chunk.get(1).copied().unwrap_or_default()) % ADDRESSES.len()];
        let gas =
            GAS_LIMITS[usize::from(chunk.get(2).copied().unwrap_or_default()) % GAS_LIMITS.len()];
        let value = VALUES[usize::from(chunk.get(3).copied().unwrap_or_default()) % VALUES.len()];

        match selector % 6 {
            0 => emit_call(&mut code, CALL, address, gas, value),
            1 => emit_call(&mut code, CALLCODE, address, gas, value),
            2 if spec.enables(SpecId::HOMESTEAD) => {
                emit_call(&mut code, DELEGATECALL, address, gas, 0)
            }
            3 if spec.enables(SpecId::BYZANTIUM) => {
                emit_call(&mut code, STATICCALL, address, gas, 0)
            }
            4 => {
                push_address(&mut code, address);
                code.push(SELFDESTRUCT);
            }
            _ => code.push(STOP),
        }
    }
    code.push(STOP);
    code
}

fn emit_call(code: &mut Vec<u8>, opcode: u8, address: [u8; 20], gas: u64, value: u64) {
    push_u64(code, 0);
    push_u64(code, 0);
    push_u64(code, 0);
    push_u64(code, 0);
    if opcode == CALL || opcode == CALLCODE {
        push_u64(code, value);
    }
    push_address(code, address);
    push_u64(code, gas);
    code.push(opcode);
    code.push(POP);
}

fn push_address(code: &mut Vec<u8>, address: [u8; 20]) {
    code.push(0x73);
    code.extend(address);
}

fn push_u64(code: &mut Vec<u8>, value: u64) {
    if value == 0 {
        code.extend([0x60, 0]);
        return;
    }
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|byte| *byte != 0).unwrap_or(bytes.len() - 1);
    code.push(0x5f + (bytes.len() - start) as u8);
    code.extend_from_slice(&bytes[start..]);
}
