use crate::{precompile, rng::Gen};
use alloy_primitives::{Address, Bytes, U256};
use evm2::{SpecId, interpreter::op};
use std::collections::BTreeSet;

pub(crate) struct Program {
    code: Vec<u8>,
    stack_height: usize,
    features: BTreeSet<&'static str>,
}

impl Program {
    pub(crate) fn generate(
        rng: &mut Gen,
        spec: SpecId,
        addresses: &[Address],
        call_addresses: &[Address],
    ) -> Self {
        let mut program = Self { code: Vec::new(), stack_height: 0, features: BTreeSet::new() };
        let statements = rng.range_inclusive(1, 48);
        for _ in 0..statements {
            match rng.range(100) {
                0..=24 => program.arithmetic(rng, spec),
                25..=39 => program.memory(rng, spec),
                40..=55 => program.storage(rng),
                56..=68 => program.environment(rng, spec),
                69..=73 => program.calldata(rng),
                74..=79 => program.external_account(rng, spec, addresses),
                80..=83 => program.log(rng),
                84..=86 if program.allow_fork_feature(rng, spec, SpecId::SPURIOUS_DRAGON) => {
                    program.precompile_call(rng, spec)
                }
                84..=86 => program.literal(rng, spec),
                87..=89 if program.allow_fork_feature(rng, spec, SpecId::SPURIOUS_DRAGON) => {
                    program.generic_call(rng, spec, call_addresses)
                }
                87..=89 => program.literal(rng, spec),
                90..=91 if program.allow_fork_feature(rng, spec, SpecId::SPURIOUS_DRAGON) => {
                    program.returndata(rng, spec, call_addresses)
                }
                90..=91 => program.literal(rng, spec),
                92..=93 if program.allow_fork_feature(rng, spec, SpecId::SPURIOUS_DRAGON) => {
                    program.create(rng, spec)
                }
                92..=93 => program.literal(rng, spec),
                94..=95 => program.cancun(rng, spec),
                96 => program.jump(rng),
                97 if rng.one_in(3) => program.stack_shuffle(rng, spec),
                97 if rng.one_in(2) => program.selfdestruct(rng, addresses),
                97 => program.stack_cleanup(),
                98 if program.allow_fork_feature(rng, spec, SpecId::SPURIOUS_DRAGON) => {
                    program.raw_invalidish(rng)
                }
                98 => program.literal(rng, spec),
                _ => program.literal(rng, spec),
            }
            while program.stack_height > 16 {
                program.emit(op::POP, 1, 0);
            }
        }
        match rng.range(10) {
            0 => program.finish_return(false),
            1 if program.allow_fork_feature(rng, spec, SpecId::BYZANTIUM) => {
                program.finish_return(true)
            }
            _ => program.emit(op::STOP, 0, 0),
        }
        program
    }

    pub(crate) fn into_parts(self) -> (Bytes, Vec<String>) {
        (self.code.into(), self.features.into_iter().map(str::to_string).collect())
    }

    fn allow_fork_feature(&mut self, rng: &mut Gen, spec: SpecId, since: SpecId) -> bool {
        if spec.enables(since) {
            return true;
        }
        if rng.one_in(20) {
            self.mark("fork_invalid_opcode");
            return true;
        }
        false
    }

    fn mark(&mut self, feature: &'static str) {
        self.features.insert(feature);
    }

    fn arithmetic(&mut self, rng: &mut Gen, spec: SpecId) {
        match rng.range(24) {
            0..=16 => {
                self.push_word(rng.biased_word());
                self.push_word(rng.biased_word());
                let mut ops = vec![
                    op::ADD,
                    op::MUL,
                    op::SUB,
                    op::DIV,
                    op::SDIV,
                    op::MOD,
                    op::SMOD,
                    op::EXP,
                    op::SIGNEXTEND,
                    op::LT,
                    op::GT,
                    op::SLT,
                    op::SGT,
                    op::EQ,
                    op::AND,
                    op::OR,
                    op::XOR,
                    op::BYTE,
                ];
                if self.allow_fork_feature(rng, spec, SpecId::ISTANBUL) {
                    ops.extend([op::SHL, op::SHR, op::SAR]);
                }
                self.emit(rng.pick(&ops), 2, 1);
            }
            17..=18 => {
                self.push_word(rng.biased_word());
                let opcode = if self.allow_fork_feature(rng, spec, SpecId::OSAKA) {
                    rng.pick(&[op::ISZERO, op::NOT, op::CLZ])
                } else if rng.one_in(2) {
                    op::ISZERO
                } else {
                    op::NOT
                };
                self.emit(opcode, 1, 1);
            }
            _ => {
                self.push_word(rng.biased_word());
                self.push_word(rng.biased_word());
                self.push_word(rng.biased_word());
                self.emit(if rng.one_in(2) { op::ADDMOD } else { op::MULMOD }, 3, 1);
            }
        }
    }

    fn memory(&mut self, rng: &mut Gen, spec: SpecId) {
        let offset = rng.pick(&[0, 1, 31, 32, 33, 64, 96, 128]);
        match rng.range(8) {
            0 => {
                self.push_word(rng.biased_word());
                self.push_u64(offset);
                self.emit(op::MSTORE, 2, 0);
            }
            1 => {
                self.push_u64(offset);
                self.emit(op::MLOAD, 1, 1);
            }
            2 => {
                self.push_word(rng.biased_word());
                self.push_u64(offset);
                self.emit(op::MSTORE8, 2, 0);
            }
            3 => self.emit(op::MSIZE, 0, 1),
            4 => {
                self.push_word(rng.biased_word());
                self.push_u64(offset);
                self.emit(op::MSTORE, 2, 0);
                self.push_u64(rng.pick(&[0, 1, 31, 32, 64]));
                self.push_u64(offset);
                self.emit(op::KECCAK256, 2, 1);
            }
            5 => {
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32, 64]));
                self.push_u64(rng.pick(&[0, 1, 31, 32, 64]));
                self.push_u64(offset);
                self.emit(op::CODECOPY, 3, 0);
            }
            6 if self.allow_fork_feature(rng, spec, SpecId::CANCUN) => {
                self.push_u64(rng.pick(&[0, 1, 31, 32, 64]));
                self.push_u64(rng.pick(&[0, 32, 64, 96]));
                self.push_u64(offset);
                self.emit(op::MCOPY, 3, 0);
            }
            _ => self.emit(op::PC, 0, 1),
        }
    }

    fn storage(&mut self, rng: &mut Gen) {
        let key = rng.biased_word();
        if rng.one_in(2) {
            self.push_word(rng.biased_word());
            self.push_word(key);
            self.emit(op::SSTORE, 2, 0);
        } else {
            self.push_word(key);
            self.emit(op::SLOAD, 1, 1);
        }
    }

    fn environment(&mut self, rng: &mut Gen, spec: SpecId) {
        match rng.range(8) {
            0..=4 => {
                let mut ops = vec![
                    op::ADDRESS,
                    op::ORIGIN,
                    op::CALLER,
                    op::CALLVALUE,
                    op::CALLDATASIZE,
                    op::CODESIZE,
                    op::GASPRICE,
                    op::COINBASE,
                    op::TIMESTAMP,
                    op::NUMBER,
                    op::DIFFICULTY,
                    op::GASLIMIT,
                    op::GAS,
                ];
                if self.allow_fork_feature(rng, spec, SpecId::ISTANBUL) {
                    ops.push(op::CHAINID);
                }
                if self.allow_fork_feature(rng, spec, SpecId::LONDON) {
                    ops.push(op::BASEFEE);
                }
                if self.allow_fork_feature(rng, spec, SpecId::AMSTERDAM) {
                    ops.push(op::SLOTNUM);
                }
                self.emit(rng.pick(&ops), 0, 1);
            }
            _ => {
                self.push_word(rng.small_word(1_000_000));
                self.emit(op::BLOCKHASH, 1, 1);
            }
        }
    }

    fn calldata(&mut self, rng: &mut Gen) {
        match rng.range(3) {
            0 => {
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32, 64]));
                self.emit(op::CALLDATALOAD, 1, 1);
            }
            1 => self.emit(op::CALLDATASIZE, 0, 1),
            _ => {
                self.push_u64(rng.pick(&[0, 32, 64]));
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32]));
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32]));
                self.emit(op::CALLDATACOPY, 3, 0);
            }
        }
    }

    fn external_account(&mut self, rng: &mut Gen, spec: SpecId, addresses: &[Address]) {
        let address = rng.pick(addresses);
        match rng.range(5) {
            0 => {
                self.push_address(address);
                self.emit(op::BALANCE, 1, 1);
            }
            1 => {
                self.push_address(address);
                self.emit(op::EXTCODESIZE, 1, 1);
            }
            2 if self.allow_fork_feature(rng, spec, SpecId::ISTANBUL) => {
                self.push_address(address);
                self.emit(op::EXTCODEHASH, 1, 1);
            }
            3 => {
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32, 33, 64]));
                self.push_u64(rng.pick(&[0, 1, 31, 32]));
                self.push_u64(rng.pick(&[0, 1, 31, 32, 64]));
                self.push_address(address);
                self.emit(op::EXTCODECOPY, 4, 0);
            }
            _ if self.allow_fork_feature(rng, spec, SpecId::ISTANBUL) => {
                self.emit(op::SELFBALANCE, 0, 1)
            }
            _ => {
                self.push_address(address);
                self.emit(op::BALANCE, 1, 1);
            }
        }
    }

    fn log(&mut self, rng: &mut Gen) {
        if rng.one_in(2) {
            self.push_word(rng.biased_word());
            self.push_u64(rng.pick(&[0, 1, 31, 32, 33, 64, 96]));
            self.emit(op::MSTORE, 2, 0);
        }
        let topics = rng.range(5);
        let mut topic_values = Vec::with_capacity(topics);
        for _ in 0..topics {
            topic_values.push(rng.biased_word());
        }
        for topic in topic_values.into_iter().rev() {
            self.push_word(topic);
        }
        self.push_u64(rng.pick(&[0, 1, 2, 31, 32, 33, 64]));
        self.push_u64(rng.pick(&[0, 1, 30, 31, 32, 64, 96]));
        self.emit(op::LOG0 + topics as u8, 2 + topics, 0);
    }

    fn precompile_call(&mut self, rng: &mut Gen, spec: SpecId) {
        let precompile = precompile::random_target(rng, spec);
        let input = precompile::input(rng, precompile);
        self.mark("precompile_call");
        self.mark(precompile.feature());
        self.mark(match input.shape {
            "empty" => "precompile_input_empty",
            "exact" => "precompile_input_exact",
            "short" => "precompile_input_short",
            "long" => "precompile_input_long",
            _ => "precompile_input_arbitrary",
        });
        if !precompile.is_enabled(spec) {
            self.mark("precompile_future_address");
        }

        let input_offset: u64 = rng.pick(&[0, 32, 64, 96]);
        self.write_memory(input_offset, &input.bytes);

        let return_offset: u64 = rng.pick(&[0, 32, 64, 128]);
        let return_len: u64 = rng.pick(&[0, 1, 20, 32, 64, 96, 128, 256]);
        let gas: u64 = rng.pick(&[0, 1, 3_000, 10_000, 50_000, 200_000, 2_000_000]);
        if self.allow_fork_feature(rng, spec, SpecId::BYZANTIUM) && rng.one_in(2) {
            self.mark("precompile_staticcall");
            self.push_u64(return_len);
            self.push_u64(return_offset);
            self.push_u64(input.bytes.len() as u64);
            self.push_u64(input_offset);
            self.push_address(precompile.address());
            self.push_u64(gas);
            self.emit(op::STATICCALL, 6, 1);
        } else {
            self.mark("precompile_call_op");
            self.push_u64(return_len);
            self.push_u64(return_offset);
            self.push_u64(input.bytes.len() as u64);
            self.push_u64(input_offset);
            self.push_u64(0);
            self.push_address(precompile.address());
            self.push_u64(gas);
            self.emit(op::CALL, 7, 1);
        }
    }

    fn generic_call(&mut self, rng: &mut Gen, spec: SpecId, addresses: &[Address]) {
        let address = rng.pick(addresses);
        let input_offset: u64 = rng.pick(&[0, 32, 64, 96]);
        let input_len: u64 = rng.pick(&[0, 1, 4, 31, 32, 64]);
        let words = input_len.div_ceil(32);
        for word in 0..words {
            self.push_word(rng.biased_word());
            self.push_u64(input_offset + word * 32);
            self.emit(op::MSTORE, 2, 0);
        }
        let return_offset: u64 = rng.pick(&[0, 32, 64, 128]);
        let return_len: u64 = rng.pick(&[0, 1, 4, 32, 64]);
        let gas: u64 = rng.pick(&[0, 1, 2_300, 10_000, 50_000, 200_000]);
        match rng.range(4) {
            0 if self.allow_fork_feature(rng, spec, SpecId::BYZANTIUM) => {
                self.push_u64(return_len);
                self.push_u64(return_offset);
                self.push_u64(input_len);
                self.push_u64(input_offset);
                self.push_address(address);
                self.push_u64(gas);
                self.emit(op::STATICCALL, 6, 1);
            }
            1 if self.allow_fork_feature(rng, spec, SpecId::HOMESTEAD) => {
                self.push_u64(return_len);
                self.push_u64(return_offset);
                self.push_u64(input_len);
                self.push_u64(input_offset);
                self.push_address(address);
                self.push_u64(gas);
                self.emit(op::DELEGATECALL, 6, 1);
            }
            2 => {
                self.push_u64(return_len);
                self.push_u64(return_offset);
                self.push_u64(input_len);
                self.push_u64(input_offset);
                self.push_u64(rng.pick(&[0, 0, 0, 1, 2]));
                self.push_address(address);
                self.push_u64(gas);
                self.emit(op::CALLCODE, 7, 1);
            }
            _ => {
                self.push_u64(return_len);
                self.push_u64(return_offset);
                self.push_u64(input_len);
                self.push_u64(input_offset);
                self.push_u64(rng.pick(&[0, 0, 0, 1, 2]));
                self.push_address(address);
                self.push_u64(gas);
                self.emit(op::CALL, 7, 1);
            }
        }
    }

    fn returndata(&mut self, rng: &mut Gen, spec: SpecId, call_addresses: &[Address]) {
        if !self.allow_fork_feature(rng, spec, SpecId::BYZANTIUM) {
            self.literal(rng, spec);
            return;
        }
        if rng.one_in(2) {
            self.generic_call(rng, spec, call_addresses);
        }
        match rng.range(3) {
            0 => self.emit(op::RETURNDATASIZE, 0, 1),
            _ => {
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32, 64]));
                self.push_u64(rng.pick(&[0, 1, 4, 31, 32, 64]));
                self.push_u64(rng.pick(&[0, 1, 31, 32, 64, 128]));
                self.emit(op::RETURNDATACOPY, 3, 0);
            }
        }
    }

    fn cancun(&mut self, rng: &mut Gen, spec: SpecId) {
        if !self.allow_fork_feature(rng, spec, SpecId::CANCUN) {
            self.literal(rng, spec);
            return;
        }
        match rng.range(4) {
            0 => {
                self.push_word(rng.biased_word());
                self.emit(op::TLOAD, 1, 1);
            }
            1 => {
                self.push_word(rng.biased_word());
                self.push_word(rng.biased_word());
                self.emit(op::TSTORE, 2, 0);
            }
            2 => {
                self.push_u64(rng.pick(&[0, 1, 2, 255]));
                self.emit(op::BLOBHASH, 1, 1);
            }
            _ => self.emit(op::BLOBBASEFEE, 0, 1),
        }
    }

    fn create(&mut self, rng: &mut Gen, spec: SpecId) {
        let initcode = match rng.range(5) {
            0 => Vec::new(),
            1 => vec![op::STOP],
            2 => vec![op::PUSH1, 0, op::PUSH1, 0, op::RETURN],
            3 if self.allow_fork_feature(rng, spec, SpecId::BYZANTIUM) => {
                vec![op::PUSH1, 0, op::PUSH1, 0, op::REVERT]
            }
            _ => vec![
                op::PUSH1,
                0,
                op::PUSH1,
                0,
                op::MSTORE,
                op::PUSH1,
                1,
                op::PUSH1,
                31,
                op::RETURN,
            ],
        };
        let offset: u64 = rng.pick(&[0, 32, 64]);
        if !initcode.is_empty() {
            self.push_bytes_word(&initcode);
            self.push_u64(offset);
            self.emit(op::MSTORE, 2, 0);
        }
        if rng.one_in(2) || !self.allow_fork_feature(rng, spec, SpecId::ISTANBUL) {
            self.push_u64(initcode.len() as u64);
            self.push_u64(offset);
            self.push_u64(rng.pick(&[0, 0, 0, 1]));
            self.emit(op::CREATE, 3, 1);
        } else {
            self.push_word(rng.biased_word());
            self.push_u64(initcode.len() as u64);
            self.push_u64(offset);
            self.push_u64(rng.pick(&[0, 0, 0, 1]));
            self.emit(op::CREATE2, 4, 1);
        }
    }

    fn jump(&mut self, rng: &mut Gen) {
        match rng.range(10) {
            0 => self.invalid_jump(rng),
            1 => self.invalid_jumpi(rng),
            2..=5 => self.forward_jump(rng),
            _ => self.forward_jumpi(rng),
        }
    }

    fn invalid_jump(&mut self, rng: &mut Gen) {
        self.push_word(rng.biased_invalid_jumpdest());
        self.emit(op::JUMP, 1, 0);
    }

    fn invalid_jumpi(&mut self, rng: &mut Gen) {
        self.push_u8(if rng.one_in(3) { 0 } else { 1 });
        self.push_word(rng.biased_invalid_jumpdest());
        self.emit(op::JUMPI, 2, 0);
    }

    fn forward_jump(&mut self, rng: &mut Gen) {
        let dest = self.push_jump_placeholder();
        self.emit(op::JUMP, 1, 0);
        self.skipped_block(rng);
        self.patch_jump(dest);
        self.emit(op::JUMPDEST, 0, 0);
    }

    fn forward_jumpi(&mut self, rng: &mut Gen) {
        self.push_u8(if rng.one_in(2) { 0 } else { 1 });
        let dest = self.push_jump_placeholder();
        self.emit(op::JUMPI, 2, 0);
        self.skipped_block(rng);
        self.patch_jump(dest);
        self.emit(op::JUMPDEST, 0, 0);
    }

    fn skipped_block(&mut self, rng: &mut Gen) {
        for _ in 0..rng.range_inclusive(1, 4) {
            self.stack_neutral_statement(rng);
        }
    }

    fn stack_neutral_statement(&mut self, rng: &mut Gen) {
        match rng.range(4) {
            0 => {
                self.push_word(rng.biased_word());
                self.emit(op::POP, 1, 0);
            }
            1 => {
                self.push_word(rng.biased_word());
                self.push_u64(rng.pick(&[0, 1, 31, 32, 33, 64, 96, 128]));
                self.emit(op::MSTORE, 2, 0);
            }
            2 => {
                self.push_word(rng.biased_word());
                self.push_word(rng.biased_word());
                self.emit(op::SSTORE, 2, 0);
            }
            _ => self.emit(op::JUMPDEST, 0, 0),
        }
    }

    fn stack_shuffle(&mut self, rng: &mut Gen, spec: SpecId) {
        while self.stack_height < 2 {
            self.push_word(rng.biased_word());
        }
        if self.allow_fork_feature(rng, spec, SpecId::AMSTERDAM) && rng.one_in(3) {
            self.relative_stack_shuffle(rng);
        } else if rng.one_in(2) {
            let n = rng.range_inclusive(1, self.stack_height.min(16));
            self.emit(op::DUP1 + n as u8 - 1, 0, 1);
        } else {
            let n = rng.range_inclusive(1, (self.stack_height - 1).min(16));
            self.emit(op::SWAP1 + n as u8 - 1, 0, 0);
        }
    }

    fn relative_stack_shuffle(&mut self, rng: &mut Gen) {
        match rng.range(3) {
            0 => {
                while self.stack_height < 18 {
                    self.push_word(rng.biased_word());
                }
                self.emit_with_immediate(op::DUPN, 0x80, 0, 1);
            }
            1 => {
                while self.stack_height < 18 {
                    self.push_word(rng.biased_word());
                }
                self.emit_with_immediate(op::SWAPN, 0x80, 0, 0);
            }
            _ => {
                while self.stack_height < 3 {
                    self.push_word(rng.biased_word());
                }
                self.emit_with_immediate(op::EXCHANGE, 0x8e, 0, 0);
            }
        }
    }

    fn selfdestruct(&mut self, rng: &mut Gen, addresses: &[Address]) {
        self.push_address(rng.pick(addresses));
        self.emit(op::SELFDESTRUCT, 1, 0);
    }

    fn stack_cleanup(&mut self) {
        if self.stack_height == 0 {
            self.push_u64(0);
        }
        self.emit(op::POP, 1, 0);
    }

    fn raw_invalidish(&mut self, rng: &mut Gen) {
        if rng.one_in(3) {
            self.mark("truncated_push");
            self.code.push(op::PUSH4);
            let immediate_len = rng.range_inclusive(0, 3);
            self.code.extend(rng.bytes(immediate_len));
            self.stack_height += 1;
        } else {
            self.emit(0xfe, 0, 0);
        }
    }

    fn write_memory(&mut self, offset: u64, bytes: &[u8]) {
        for (index, chunk) in bytes.chunks(32).enumerate() {
            self.push_bytes_word(chunk);
            self.push_u64(offset + (index as u64) * 32);
            self.emit(op::MSTORE, 2, 0);
        }
    }

    fn literal(&mut self, rng: &mut Gen, spec: SpecId) {
        if self.allow_fork_feature(rng, spec, SpecId::SHANGHAI) && rng.one_in(8) {
            self.emit(op::PUSH0, 0, 1);
        } else if rng.one_in(8) {
            self.push_random_width_word(rng);
        } else {
            self.push_word(rng.biased_word());
        }
    }

    fn finish_return(&mut self, revert: bool) {
        self.push_u64(0);
        self.push_u64(0);
        self.emit(if revert { op::REVERT } else { op::RETURN }, 2, 0);
    }

    fn push_random_width_word(&mut self, rng: &mut Gen) {
        let len = rng.range_inclusive(1, 32);
        self.mark("push");
        self.mark("wide_push");
        self.code.push(op::PUSH1 + len as u8 - 1);
        self.code.extend(rng.bytes(len));
        self.stack_height += 1;
    }

    fn push_address(&mut self, address: Address) {
        self.push_word(U256::from_be_slice(address.as_slice()));
    }

    fn push_bytes_word(&mut self, bytes: &[u8]) {
        let mut word = [0; 32];
        word[..bytes.len()].copy_from_slice(bytes);
        self.push_word(U256::from_be_bytes(word));
    }

    fn push_word(&mut self, value: U256) {
        if value <= U256::from(u8::MAX) {
            self.push_u8(value.to::<u8>());
        } else if value <= U256::from(u64::MAX) {
            self.push_u64(value.to::<u64>());
        } else {
            self.mark("push");
            self.code.push(op::PUSH32);
            self.code.extend(value.to_be_bytes::<32>());
            self.stack_height += 1;
        }
    }

    fn push_u8(&mut self, value: u8) {
        self.mark("push");
        self.code.extend([op::PUSH1, value]);
        self.stack_height += 1;
    }

    fn push_u64(&mut self, value: u64) {
        if value <= u64::from(u8::MAX) {
            self.push_u8(value as u8);
            return;
        }
        let bytes = value.to_be_bytes();
        let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(bytes.len() - 1);
        let immediate = &bytes[first..];
        self.mark("push");
        self.code.push(op::PUSH1 + immediate.len() as u8 - 1);
        self.code.extend(immediate);
        self.stack_height += 1;
    }

    fn push_jump_placeholder(&mut self) -> usize {
        self.mark("push");
        self.code.push(op::PUSH2);
        let immediate = self.code.len();
        self.code.extend([0, 0]);
        self.stack_height += 1;
        immediate
    }

    fn patch_jump(&mut self, immediate: usize) {
        let dest = self.code.len() as u16;
        self.code[immediate..immediate + 2].copy_from_slice(&dest.to_be_bytes());
    }

    fn emit(&mut self, opcode: u8, pops: usize, pushes: usize) {
        self.mark_opcode(opcode);
        self.code.push(opcode);
        self.stack_height = self.stack_height.saturating_sub(pops) + pushes;
    }

    fn emit_with_immediate(&mut self, opcode: u8, immediate: u8, pops: usize, pushes: usize) {
        self.mark_opcode(opcode);
        self.code.extend([opcode, immediate]);
        self.stack_height = self.stack_height.saturating_sub(pops) + pushes;
    }

    fn mark_opcode(&mut self, opcode: u8) {
        match opcode {
            op::SDIV | op::SMOD | op::SLT | op::SGT | op::SAR | op::SIGNEXTEND => {
                self.mark("signed_arithmetic")
            }
            op::CLZ => self.mark("clz"),
            op::ADDMOD | op::MULMOD => self.mark("modular_arithmetic"),
            op::SHL | op::SHR => self.mark("shift"),
            op::KECCAK256 => self.mark("keccak256"),
            op::MLOAD | op::MSTORE | op::MSTORE8 | op::MSIZE => self.mark("memory"),
            op::CODECOPY | op::CALLDATACOPY | op::EXTCODECOPY | op::MCOPY => self.mark("copy"),
            op::SLOAD | op::SSTORE => self.mark("storage"),
            op::BLOCKHASH | op::ORIGIN | op::DIFFICULTY | op::CHAINID | op::BASEFEE => {
                self.mark("environment")
            }
            op::SLOTNUM => self.mark("slotnum"),
            op::BALANCE | op::EXTCODESIZE | op::EXTCODEHASH | op::SELFBALANCE => {
                self.mark("external_account")
            }
            op::LOG0..=op::LOG4 => self.mark("log"),
            op::CALL => self.mark("call"),
            op::STATICCALL => self.mark("staticcall"),
            op::DELEGATECALL => self.mark("delegatecall"),
            op::CALLCODE => self.mark("callcode"),
            op::CREATE => self.mark("create"),
            op::CREATE2 => self.mark("create2"),
            op::RETURNDATASIZE | op::RETURNDATACOPY => self.mark("returndata"),
            op::TLOAD | op::TSTORE => self.mark("transient_storage"),
            op::BLOBHASH | op::BLOBBASEFEE => self.mark("blob"),
            op::JUMP | op::JUMPI | op::JUMPDEST => self.mark("jump"),
            op::DUP1..=op::DUP16 | op::SWAP1..=op::SWAP16 => self.mark("dup_swap"),
            op::DUPN | op::SWAPN | op::EXCHANGE => self.mark("relative_stack"),
            op::PUSH0 => {
                self.mark("push");
                self.mark("push0");
            }
            op::REVERT => self.mark("revert"),
            op::RETURN => self.mark("return"),
            op::INVALID => self.mark("invalid"),
            op::SELFDESTRUCT => self.mark("selfdestruct"),
            _ => {}
        }
    }
}
