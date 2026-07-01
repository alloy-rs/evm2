#![no_main]

mod common;
mod precompile_case;

use arbitrary::{Arbitrary, Dearbitrary, Unstructured};
use evm2_fuzzer::bytecode_case_with_spec;
use libfuzzer_sys::{fuzz_mutator, fuzz_target, fuzzer_mutate};
use precompile_case::{PrecompileAddress, PrecompileCase};
use rand::{RngExt, SeedableRng, rngs::StdRng};

const STOP: u8 = 0x00;
const POP: u8 = 0x50;
const MSTORE: u8 = 0x52;
const RETURN: u8 = 0xf3;
const CALL: u8 = 0xf1;
const STATICCALL: u8 = 0xfa;

fuzz_target!(|data: &[u8]| {
    let spec = common::target_spec("precompile_compare_");
    let mut input = Unstructured::new(data);
    let Ok(case) = PrecompileCase::arbitrary(&mut input) else {
        return;
    };

    let bytecode = precompile_program(
        case.address.number(),
        case.gas,
        &case.input,
        u64::from(case.return_len),
        case.is_static,
    );
    common::run_case(bytecode_case_with_spec(spec, &bytecode));
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    mutate_precompile_case(data, size, max_size, seed)
});

fn mutate_precompile_case(data: &mut [u8], size: usize, max_size: usize, seed: u32) -> usize {
    let mut rng = StdRng::seed_from_u64(u64::from(seed));
    let current = data.get(..size).and_then(parse_case);
    let mut case = if one_in(&mut rng, 8) {
        default_case(&mut rng)
    } else {
        current.unwrap_or_else(|| default_case(&mut rng))
    };

    match range(&mut rng, 7) {
        0 => case.address = PrecompileAddress::from_index(random_usize(&mut rng)),
        1 => case.gas = rng.random(),
        2 => case.return_len = rng.random(),
        3 => case.is_static = !case.is_static,
        4 => replace_input(&mut case.input, &mut rng),
        5 => mutate_input_byte(&mut case.input, &mut rng),
        _ => truncate_or_extend_input(&mut case.input, &mut rng),
    }

    let bytes = case.to_arbitrary_bytes();
    if bytes.len() <= max_size {
        data[..bytes.len()].copy_from_slice(&bytes);
        bytes.len()
    } else {
        fuzzer_mutate(data, size, max_size)
    }
}

fn parse_case(data: &[u8]) -> Option<PrecompileCase> {
    let mut input = Unstructured::new(data);
    PrecompileCase::arbitrary(&mut input).ok()
}

fn default_case(rng: &mut StdRng) -> PrecompileCase {
    PrecompileCase {
        address: PrecompileAddress::from_index(random_usize(rng)),
        gas: pick(rng, &[0, 1, 20, 60, 3_000, 30_000, 500_000, 2_000_000]),
        return_len: pick(rng, &[0, 1, 20, 32, 64, 128, 512]),
        is_static: !one_in(rng, 4),
        input: Vec::new(),
    }
}

fn replace_input(input: &mut Vec<u8>, rng: &mut StdRng) {
    let len = range(rng, 512);
    input.clear();
    input.resize(len, 0);
    for byte in input {
        *byte = rng.random();
    }
}

fn mutate_input_byte(input: &mut Vec<u8>, rng: &mut StdRng) {
    if input.is_empty() {
        input.push(rng.random());
        return;
    }

    let index = range(rng, input.len());
    match range(rng, 3) {
        0 => input[index] = rng.random(),
        1 => input[index] = input[index].wrapping_add(rng.random::<u8>() | 1),
        _ => input[index] ^= 1 << range(rng, 8),
    }
}

fn truncate_or_extend_input(input: &mut Vec<u8>, rng: &mut StdRng) {
    if input.is_empty() || one_in(rng, 2) {
        let extra = 1 + range(rng, 64);
        for _ in 0..extra {
            input.push(rng.random());
        }
    } else {
        input.truncate(range(rng, input.len()));
    }
}

fn precompile_program(
    address: u64,
    gas: u64,
    input: &[u8],
    return_len: u64,
    is_static: bool,
) -> Vec<u8> {
    let mut code = Vec::new();
    let input_offset = 0_u64;
    let return_offset = 0x100_u64;

    for (index, chunk) in input.chunks(32).enumerate() {
        let mut word = [0u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        push_bytes(&mut code, &word);
        push_u64(&mut code, input_offset + (index as u64) * 32);
        code.push(MSTORE);
    }

    push_u64(&mut code, return_len);
    push_u64(&mut code, return_offset);
    push_u64(&mut code, input.len() as u64);
    push_u64(&mut code, input_offset);
    if !is_static {
        push_u64(&mut code, 0);
    }
    push_u64(&mut code, address);
    push_u64(&mut code, gas);
    code.push(if is_static { STATICCALL } else { CALL });
    code.push(POP);

    if return_len == 0 {
        code.push(STOP);
    } else {
        push_u64(&mut code, return_len);
        push_u64(&mut code, return_offset);
        code.push(RETURN);
    }
    code
}

fn push_u64(code: &mut Vec<u8>, value: u64) {
    if value == 0 {
        code.extend([0x60, 0]);
        return;
    }
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|byte| *byte != 0).unwrap_or(bytes.len() - 1);
    push_bytes(code, &bytes[start..]);
}

fn push_bytes(code: &mut Vec<u8>, bytes: &[u8]) {
    debug_assert!(!bytes.is_empty() && bytes.len() <= 32);
    code.push(0x5f + bytes.len() as u8);
    code.extend_from_slice(bytes);
}

fn random_usize(rng: &mut StdRng) -> usize {
    rng.random::<u64>() as usize
}

fn range(rng: &mut StdRng, upper: usize) -> usize {
    if upper == 0 { 0 } else { rng.random_range(..upper) }
}

fn one_in(rng: &mut StdRng, divisor: usize) -> bool {
    range(rng, divisor) == 0
}

fn pick<T: Copy>(rng: &mut StdRng, values: &[T]) -> T {
    values[range(rng, values.len())]
}
