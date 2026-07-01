use libfuzzer_sys::{fuzz_mutator, fuzzer_mutate};
use rand::{RngExt, SeedableRng, rngs::StdRng};

const PUSH1: u8 = 0x60;
const PUSH32: u8 = 0x7f;
const DUPN: u8 = 0xe6;
const SWAPN: u8 = 0xe7;
const EXCHANGE: u8 = 0xe8;

const MAX_OPS: usize = 256;

const OPCODES: &[u8] = &[
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x10, 0x11, 0x12, 0x13,
    0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x20, 0x30, 0x31, 0x32, 0x33, 0x34,
    0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x44,
    0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
    0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98,
    0x99, 0x9a, 0x9b, 0x9c, 0x9d, 0x9e, 0x9f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xe6, 0xe7, 0xe8, 0xf0,
    0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xfa, 0xfd, 0xfe, 0xff,
];

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    mutate_bytecode(data, size, max_size, seed)
});

fn mutate_bytecode(data: &mut [u8], size: usize, max_size: usize, seed: u32) -> usize {
    let mut rng = StdRng::seed_from_u64(u64::from(seed));
    let Some(bytes) = data.get(..size) else {
        return 0;
    };
    let mut code = parse_bytecode(bytes);

    if code.is_empty() {
        code.push(random_instruction(&mut rng));
    }

    match range(&mut rng, 7) {
        0 => replace_opcode(&mut code, &mut rng),
        1 => mutate_immediate(&mut code, &mut rng),
        2 if code.len() < MAX_OPS => insert_instruction(&mut code, &mut rng),
        3 => remove_instruction(&mut code, &mut rng),
        4 => duplicate_instruction(&mut code, &mut rng),
        5 => resize_push(&mut code, &mut rng),
        _ => shuffle_instruction(&mut code, &mut rng),
    }

    let bytes = serialize_bytecode(&code);
    if bytes.len() <= max_size {
        data[..bytes.len()].copy_from_slice(&bytes);
        bytes.len()
    } else {
        fuzzer_mutate(data, size, max_size)
    }
}

fn parse_bytecode(bytes: &[u8]) -> Vec<Instruction> {
    let mut pc = 0;
    let mut code = Vec::new();
    while pc < bytes.len() && code.len() < MAX_OPS {
        let opcode = bytes[pc];
        pc += 1;
        let immediate_len = immediate_len(opcode);
        let available = immediate_len.min(bytes.len().saturating_sub(pc));
        let immediate = bytes[pc..pc + available].to_vec();
        pc += available;
        code.push(Instruction { opcode, immediate });
    }
    code
}

fn serialize_bytecode(code: &[Instruction]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for instruction in code {
        bytes.push(instruction.opcode);
        bytes.extend_from_slice(&instruction.immediate);
    }
    bytes
}

fn replace_opcode(code: &mut [Instruction], rng: &mut StdRng) {
    let index = range(rng, code.len());
    let opcode = random_opcode(rng);
    code[index].opcode = opcode;
    normalize_immediate(&mut code[index], rng);
}

fn mutate_immediate(code: &mut [Instruction], rng: &mut StdRng) {
    let index = range(rng, code.len());
    if immediate_len(code[index].opcode) == 0 {
        code[index] = random_push(rng);
        return;
    }
    normalize_immediate(&mut code[index], rng);
    if code[index].immediate.is_empty() {
        return;
    }

    let byte = range(rng, code[index].immediate.len());
    match range(rng, 3) {
        0 => code[index].immediate[byte] = rng.random(),
        1 => code[index].immediate[byte] = code[index].immediate[byte].wrapping_add(1),
        _ => code[index].immediate[byte] ^= 1 << range(rng, 8),
    }
}

fn insert_instruction(code: &mut Vec<Instruction>, rng: &mut StdRng) {
    let index = range(rng, code.len() + 1);
    code.insert(index, random_instruction(rng));
}

fn remove_instruction(code: &mut Vec<Instruction>, rng: &mut StdRng) {
    if code.len() <= 1 {
        code[0] = random_instruction(rng);
    } else {
        let index = range(rng, code.len());
        code.remove(index);
    }
}

fn duplicate_instruction(code: &mut Vec<Instruction>, rng: &mut StdRng) {
    if code.len() >= MAX_OPS {
        return;
    }
    let index = range(rng, code.len());
    code.insert(index, code[index].clone());
}

fn resize_push(code: &mut [Instruction], rng: &mut StdRng) {
    let index = range(rng, code.len());
    code[index].opcode = PUSH1 + range(rng, 32) as u8;
    normalize_immediate(&mut code[index], rng);
}

fn shuffle_instruction(code: &mut [Instruction], rng: &mut StdRng) {
    if code.len() < 2 {
        return;
    }
    let a = range(rng, code.len());
    let b = range(rng, code.len());
    code.swap(a, b);
}

fn random_instruction(rng: &mut StdRng) -> Instruction {
    if one_in(rng, 3) { random_push(rng) } else { random_non_push(rng) }
}

fn random_push(rng: &mut StdRng) -> Instruction {
    let opcode = PUSH1 + range(rng, 32) as u8;
    let mut instruction = Instruction { opcode, immediate: Vec::new() };
    normalize_immediate(&mut instruction, rng);
    instruction
}

fn random_non_push(rng: &mut StdRng) -> Instruction {
    let opcode = random_opcode(rng);
    let mut instruction = Instruction { opcode, immediate: Vec::new() };
    normalize_immediate(&mut instruction, rng);
    instruction
}

fn random_opcode(rng: &mut StdRng) -> u8 {
    if one_in(rng, 16) {
        rng.random()
    } else if one_in(rng, 4) {
        PUSH1 + range(rng, 32) as u8
    } else {
        OPCODES[range(rng, OPCODES.len())]
    }
}

fn normalize_immediate(instruction: &mut Instruction, rng: &mut StdRng) {
    let len = immediate_len(instruction.opcode);
    if len == 0 {
        instruction.immediate.clear();
        return;
    }

    if one_in(rng, 16) {
        instruction.immediate.truncate(range(rng, len));
        return;
    }

    instruction.immediate.resize(len, 0);
    for byte in &mut instruction.immediate {
        if one_in(rng, 4) {
            *byte = rng.random();
        }
    }
}

fn immediate_len(opcode: u8) -> usize {
    if (PUSH1..=PUSH32).contains(&opcode) {
        (opcode - PUSH1 + 1) as usize
    } else {
        match opcode {
            DUPN | SWAPN | EXCHANGE => 1,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug)]
struct Instruction {
    opcode: u8,
    immediate: Vec<u8>,
}

fn range(rng: &mut StdRng, upper: usize) -> usize {
    if upper == 0 { 0 } else { rng.random_range(..upper) }
}

fn one_in(rng: &mut StdRng, divisor: usize) -> bool {
    range(rng, divisor) == 0
}
