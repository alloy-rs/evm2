#![no_main]

use evm2_fuzzer::{
    CaseContext, Evm2Backend, EvmBackend, RevmBackend, SpecId, bytecode_case_with_spec,
    compare_case,
};
use libfuzzer_sys::fuzz_target;

const STOP: u8 = 0x00;
const POP: u8 = 0x50;
const MSTORE: u8 = 0x52;
const RETURN: u8 = 0xf3;
const CALL: u8 = 0xf1;
const STATICCALL: u8 = 0xfa;

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

const PRECOMPILE_ADDRESSES: &[u64] = &[
    0x01, 0x02, 0x03, 0x04, // Frontier
    0x05, 0x06, 0x07, 0x08, // Byzantium
    0x09, // Istanbul
    0x0a, // Cancun
    0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,  // Prague
    0x100, // Osaka P256VERIFY
];

const GAS_LIMITS: &[u64] =
    &[0, 1, 20, 60, 500, 3_000, 6_000, 10_000, 30_000, 100_000, 500_000, 2_000_000];

const RETURN_LENGTHS: &[u64] = &[0, 1, 20, 31, 32, 33, 48, 64, 96, 128, 256, 512];

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }

    let spec = NON_AMSTERDAM_SPECS[usize::from(data[0]) % NON_AMSTERDAM_SPECS.len()];
    let address = PRECOMPILE_ADDRESSES[usize::from(data[1]) % PRECOMPILE_ADDRESSES.len()];
    let gas = GAS_LIMITS[usize::from(data[2]) % GAS_LIMITS.len()];
    let return_len = RETURN_LENGTHS[usize::from(data[3]) % RETURN_LENGTHS.len()];
    let flags = data[4];
    let is_static = flags & 1 == 0;
    let input_storage;
    let input = if flags & 2 == 0 {
        &data[5..]
    } else {
        input_storage = shaped_input(address, &data[5..]);
        &input_storage
    };

    let bytecode = precompile_program(address, gas, input, return_len, is_static);
    let backends: [&dyn EvmBackend; 2] = [&RevmBackend, &Evm2Backend];
    if let Err(err) =
        compare_case(&backends, &bytecode_case_with_spec(spec, &bytecode), CaseContext::Bytes)
    {
        panic!("{err}");
    }
});

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

fn shaped_input(address: u64, data: &[u8]) -> Vec<u8> {
    match address {
        0x0b => {
            let point = padded_g1_infinity();
            [point.as_slice(), point.as_slice()].concat()
        }
        0x0c => {
            let pairs = 1 + data.first().copied().unwrap_or_default() as usize % 3;
            let mut input = Vec::with_capacity(pairs * (128 + 32));
            for pair in 0..pairs {
                input.extend(padded_g1_infinity());
                input.extend(scalar(data, pair));
            }
            input
        }
        0x0d => {
            let point = padded_g2_infinity();
            [point.as_slice(), point.as_slice()].concat()
        }
        0x0e => {
            let pairs = 1 + data.first().copied().unwrap_or_default() as usize % 2;
            let mut input = Vec::with_capacity(pairs * (256 + 32));
            for pair in 0..pairs {
                input.extend(padded_g2_infinity());
                input.extend(scalar(data, pair));
            }
            input
        }
        0x0f => {
            let pairs = 1 + data.first().copied().unwrap_or_default() as usize % 2;
            let mut input = Vec::with_capacity(pairs * (128 + 256));
            for _ in 0..pairs {
                input.extend(padded_g1_infinity());
                input.extend(padded_g2_infinity());
            }
            input
        }
        0x10 => padded_fp(data, 0).to_vec(),
        0x11 => {
            let mut input = Vec::with_capacity(128);
            input.extend(padded_fp(data, 0));
            input.extend(padded_fp(data, 48));
            input
        }
        _ => data.to_vec(),
    }
}

fn padded_fp(data: &[u8], offset: usize) -> [u8; 64] {
    let mut fp = [0u8; 64];
    fill_wrapping(&mut fp[16..], data, offset);
    fp
}

fn padded_g1_infinity() -> [u8; 128] {
    [0u8; 128]
}

fn padded_g2_infinity() -> [u8; 256] {
    [0u8; 256]
}

fn scalar(data: &[u8], index: usize) -> [u8; 32] {
    let mut scalar = [0u8; 32];
    fill_wrapping(&mut scalar, data, 1 + index * 32);
    scalar
}

fn fill_wrapping(out: &mut [u8], data: &[u8], offset: usize) {
    if data.is_empty() {
        return;
    }
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = data[(offset + index) % data.len()];
    }
}
