#![no_main]

mod common;

use arbitrary::{Arbitrary, Unstructured};
use evm2_fuzzer::{SpecId, bytecode_case_with_spec};
use libfuzzer_sys::fuzz_target;

const STOP: u8 = 0x00;
const POP: u8 = 0x50;
const CALL: u8 = 0xf1;
const CALLCODE: u8 = 0xf2;
const DELEGATECALL: u8 = 0xf4;
const STATICCALL: u8 = 0xfa;
const SELFDESTRUCT: u8 = 0xff;

const MAX_OPS: usize = 16;

#[derive(Arbitrary, Clone, Debug)]
struct LifecycleCase {
    ops: Vec<LifecycleOp>,
}

#[derive(Arbitrary, Clone, Copy, Debug)]
struct LifecycleOp {
    kind: LifecycleKind,
    address: [u8; 20],
    gas: u64,
    value: u64,
}

#[derive(Arbitrary, Clone, Copy, Debug)]
enum LifecycleKind {
    Call,
    CallCode,
    DelegateCall,
    StaticCall,
    SelfDestruct,
    Stop,
}

fuzz_target!(|data: &[u8]| {
    let spec = common::target_spec("lifecycle_compare_");
    let mut input = Unstructured::new(data);
    let Ok(case) = LifecycleCase::arbitrary(&mut input) else {
        return;
    };

    let bytecode = lifecycle_program(&case, spec);
    common::run_case(bytecode_case_with_spec(spec, &bytecode));
});

fn lifecycle_program(case: &LifecycleCase, spec: SpecId) -> Vec<u8> {
    let mut code = Vec::new();
    for op in case.ops.iter().take(MAX_OPS) {
        match op.kind {
            LifecycleKind::Call => emit_call(&mut code, CALL, op.address, op.gas, op.value),
            LifecycleKind::CallCode => emit_call(&mut code, CALLCODE, op.address, op.gas, op.value),
            LifecycleKind::DelegateCall if spec.enables(SpecId::HOMESTEAD) => {
                emit_call(&mut code, DELEGATECALL, op.address, op.gas, 0)
            }
            LifecycleKind::StaticCall if spec.enables(SpecId::BYZANTIUM) => {
                emit_call(&mut code, STATICCALL, op.address, op.gas, 0)
            }
            LifecycleKind::SelfDestruct => {
                push_address(&mut code, op.address);
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
