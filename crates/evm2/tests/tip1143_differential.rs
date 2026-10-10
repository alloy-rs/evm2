//! TIP1143-T34: bounded independent resident-byte execution oracle.
//! Supported: STOP, PUSH1..32, POP, LT, GAS, PC, CODESIZE, CODECOPY,
//! MSTORE, JUMP, JUMPI, JUMPDEST, RETURN and REVERT. Unsupported opcodes
//! panic explicitly. CALL is covered by a bounded non-value forwarding driver;
//! storage/log rollback is covered by independently priced fixed vectors.
//! The oracle never calls production range, analysis, gas or warmth helpers.
//! Ordinary-semantics reference: c5e4c6c8f73bbfd5139b46a7197bc317e36a57bb,
//! crates/evm2/src/interpreter/instructions/{control,env,system}.rs and
//! crates/evm2/src/version/gas_params.rs. The small calibration vectors below
//! pin hand-calculated ordinary gas and PC/GAS outputs; draft tariffs are then
//! applied during oracle execution. This is not a revm replay corpus or proof
//! of arbitrary nested execution/state-root equivalence. Independent execution
//! of the complete oracle vector set against that base remains unverified.

use alloy_primitives::{Address, B256, Bytes, TxKind, U256, keccak256};
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
    bytecode::{Bytecode, CodeChunk, CodeMetadata},
    env::{BlockEnvExt, TxEnvExt},
    ethereum::{execute_initial_frame, prepare_initial_frame},
    evm::{AccountInfo, Database, Db},
    interpreter::{GasTracker, InstrStop, Word},
    registry::TxRegistry,
};
use std::{cell::RefCell, collections::BTreeSet, io, rc::Rc};

const CHUNK: usize = 24540;

#[derive(Clone, Debug)]
struct Provider {
    code: Bytes,
    hash: B256,
    reads: Rc<RefCell<Vec<u32>>>,
    full_reads: Rc<RefCell<usize>>,
}

impl Database for Provider {
    type Error = io::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        if *address != Address::repeat_byte(0x44) {
            return Ok(None);
        }
        Ok(Some(AccountInfo {
            nonce: 1,
            code_hash: self.hash,
            extension: (if self.code.len() > CHUNK {
                Some(
                    CodeMetadata::new(
                        self.code.len() as u32,
                        evm2::bytecode::code_metadata(&self.code)
                            .unwrap()
                            .unwrap()
                            .chunk_hashes()
                            .to_vec(),
                    )
                    .unwrap(),
                )
            } else {
                None
            })
            .map(evm2::evm::AccountExtension::chunked)
            .unwrap_or_default(),
            ..Default::default()
        }))
    }

    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        assert_eq!(*hash, self.hash);
        *self.full_reads.borrow_mut() += 1;
        self.reads.borrow_mut().push(0);
        assert!(self.code.len() <= CHUNK, "execution reconstructed multi-chunk code");
        Ok(Bytecode::new_legacy(self.code.clone()))
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        assert_eq!(*hash, self.hash);
        self.reads.borrow_mut().push(index);
        Ok(evm2::bytecode::code_chunk(&self.code, index))
    }
}

fn execute(code: Vec<u8>, enabled: bool, budget: u64) -> (InstrStop, Bytes, u64, Vec<u32>, usize) {
    let provider = Provider {
        hash: keccak256(&code),
        code: code.into(),
        reads: Rc::default(),
        full_reads: Rc::default(),
    };
    let reads = provider.reads.clone();
    let full_reads = provider.full_reads.clone();
    let version = Version::new(SpecId::PRAGUE).with_tip1143(enabled);
    let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        SpecId::PRAGUE,
        BlockEnvExt::default(),
        TxRegistry::new(),
        Db::new(provider),
        Precompiles::base(SpecId::PRAGUE),
    );
    let mut previous = None;
    for round in 0..2 {
        let mut gas = GasTracker::new(budget);
        let frame = prepare_initial_frame(
            &mut evm,
            Address::repeat_byte(0x55),
            0,
            TxKind::Call(Address::repeat_byte(0x44)),
            &Bytes::new(),
            U256::ZERO,
            &mut gas,
        )
        .unwrap();
        let result =
            execute_initial_frame(&mut evm, &TxEnvExt::default(), frame, &mut gas, budget, 0)
                .unwrap();
        let observed = reads.borrow().clone();
        let full = *full_reads.borrow();
        let actual = (result.stop, result.output, gas.spent(), observed, full);
        if let Some(expected) = &previous {
            assert_eq!(&actual, expected, "reset/cache replay round={round}");
        }
        previous = Some(actual);
        evm.state_mut().clear_transaction_state();
    }
    previous.unwrap()
}

fn resident(code: &[u8], budget: u64) -> (InstrStop, Bytes, u64) {
    resident_frame(code, budget, true)
}

fn resident_frame(code: &[u8], budget: u64, charge_entry: bool) -> (InstrStop, Bytes, u64) {
    let mut gas = budget;
    let mut stack = Vec::<U256>::new();
    let mut memory = Vec::<u8>::new();
    let mut pc = 0usize;
    let mut active = 0usize;
    let mut warm = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    let mut scan = 0;
    while scan < code.len() {
        let op = code[scan];
        if op == 0x5b {
            destinations.insert(scan);
        }
        scan += 1 + if (0x60..=0x7f).contains(&op) { (op - 0x5f) as usize } else { 0 };
    }
    macro_rules! charge {
        ($cost:expr) => {{
            let cost = $cost;
            if gas < cost {
                return (InstrStop::OutOfGas, Bytes::new(), budget);
            }
            gas -= cost;
        }};
    }
    macro_rules! expand {
        ($end:expr) => {{
            let end: usize = $end;
            let before = memory.len().div_ceil(32) as u64;
            let after = end.div_ceil(32) as u64;
            if after > before {
                let cost = 3 * (after - before) + after * after / 512 - before * before / 512;
                if gas < cost {
                    return (InstrStop::MemoryOOG, Bytes::new(), budget);
                }
                gas -= cost;
                memory.resize(after as usize * 32, 0);
            }
        }};
    }
    if !code.is_empty() {
        if charge_entry {
            charge!(28680);
        }
        warm.insert(0);
    }
    for _ in 0..10000 {
        if pc >= code.len() {
            return (InstrStop::Stop, Bytes::new(), budget - gas);
        }
        let at = pc;
        let op = code[pc];
        pc += 1;
        match op {
            0 => return (InstrStop::Stop, Bytes::new(), budget - gas),
            0x60..=0x7f => {
                charge!(3);
                let width = (op - 0x5f) as usize;
                let mut bytes = [0u8; 32];
                for i in 0..width {
                    bytes[32 - width + i] = code.get(pc + i).copied().unwrap_or(0);
                }
                stack.push(U256::from_be_bytes(bytes));
                pc += width;
            }
            0x10 => {
                charge!(3);
                let a = stack.pop().unwrap();
                let b = stack.pop().unwrap();
                stack.push(U256::from(u8::from(a < b)));
            }
            0x50 => {
                charge!(2);
                stack.pop().unwrap();
            }
            0x58 => {
                charge!(2);
                stack.push(U256::from(at));
            }
            0x38 => {
                charge!(2);
                stack.push(U256::from(code.len()));
            }
            0x5a => {
                charge!(2);
                stack.push(U256::from(gas));
            }
            0x5b => {
                charge!(1);
            }
            0x52 => {
                charge!(3);
                let offset = usize::try_from(stack.pop().unwrap()).unwrap();
                let value = stack.pop().unwrap();
                expand!(offset.checked_add(32).unwrap());
                memory[offset..offset + 32].copy_from_slice(&value.to_be_bytes::<32>());
            }
            0x56 | 0x57 => {
                charge!(if op == 0x56 { 8 } else { 10 });
                let target = stack.pop().unwrap();
                let taken = op == 0x56 || stack.pop().unwrap() != U256::ZERO;
                if !taken {
                    continue;
                }
                if target >= U256::from(code.len()) {
                    return (InstrStop::InvalidJump, Bytes::new(), budget);
                }
                let target = usize::try_from(target).unwrap();
                let chunk = target / CHUNK;
                if chunk != active {
                    charge!(if warm.contains(&chunk) { 1000 } else { 28680 });
                    warm.insert(chunk);
                }
                if !destinations.contains(&target) {
                    return (InstrStop::InvalidJump, Bytes::new(), budget);
                }
                active = chunk;
                pc = target;
            }
            0x39 => {
                charge!(3);
                let destination = stack.pop().unwrap();
                let source = stack.pop().unwrap();
                let length = usize::try_from(stack.pop().unwrap()).unwrap();
                charge!(3 * (length as u64).div_ceil(32));
                if length == 0 {
                    continue;
                }
                let destination = usize::try_from(destination).unwrap();
                expand!(destination.checked_add(length).unwrap());
                let mut indices = Vec::new();
                if source < U256::from(code.len()) {
                    let start = usize::try_from(source).unwrap();
                    let end = start + length.min(code.len() - start);
                    indices.extend(start / CHUNK..=(end - 1) / CHUNK);
                }
                let cost = indices
                    .iter()
                    .map(|i| if warm.contains(i) { 1000u64 } else { 28680 })
                    .sum::<u64>();
                charge!(cost);
                warm.extend(indices);
                for i in 0..length {
                    let offset = source.checked_add(U256::from(i));
                    memory[destination + i] = offset
                        .and_then(|n| usize::try_from(n).ok())
                        .and_then(|n| code.get(n).copied())
                        .unwrap_or(0);
                }
            }
            0xf3 | 0xfd => {
                let offset = usize::try_from(stack.pop().unwrap()).unwrap();
                let length = usize::try_from(stack.pop().unwrap()).unwrap();
                expand!(offset.checked_add(length).unwrap());
                let output = Bytes::copy_from_slice(&memory[offset..offset + length]);
                return (
                    if op == 0xf3 { InstrStop::Return } else { InstrStop::Revert },
                    output,
                    budget - gas,
                );
            }
            _ => panic!("unsupported oracle opcode {op:#x} at {at}"),
        }
    }
    panic!("oracle instruction bound exhausted")
}

#[test]
fn tip1143_t34_resident_oracle_gas_jump_copy_and_revert() {
    let return_gas = vec![0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
    let golden = resident(&return_gas, 100000);
    assert_eq!(golden.0, InstrStop::Return);
    assert_eq!(golden.2, 28680 + 17);
    assert_eq!(U256::from_be_slice(&golden.1), U256::from(100000 - 28680 - 2));
    let mut jump = vec![0x61, (CHUNK >> 8) as u8, CHUNK as u8, 0x56];
    jump.resize(CHUNK, 0);
    jump.extend([0x5b, 0x58, 0x60, 0, 0x52, 0x5a, 0x60, 32, 0x52, 0x60, 64, 0x60, 0, 0xf3]);
    let mut copy = vec![
        0x60,
        4,
        0x61,
        ((CHUNK - 1) >> 8) as u8,
        (CHUNK - 1) as u8,
        0x60,
        0,
        0x39,
        0x60,
        4,
        0x60,
        0,
        0xfd,
    ];
    copy.resize(CHUNK, 0);
    copy.extend([0x60, 42, 0]);
    for code in [return_gas, jump, copy] {
        for budget in [28680 + 16, 57360 + 20, 100000] {
            let expected = resident(&code, budget);
            let actual = execute(code.clone(), true, budget);
            assert_eq!(
                (actual.0, actual.1, actual.2),
                expected,
                "length={}, budget={budget}",
                code.len()
            );
            assert_eq!(actual.4, usize::from(code.len() <= CHUNK));
        }
    }
}

#[test]
fn tip1143_t34_tariffs_change_control_flow_before_gas_sensitive_branch() {
    // PUSH2 50000; GAS; LT; PUSH2 remote; JUMPI. The branch observes
    // already-paid entry gas, so a post-hoc subtraction cannot satisfy this case.
    let mut code = vec![0x61, 0xc3, 0x50, 0x5a, 0x10, 0x61, (CHUNK >> 8) as u8, CHUNK as u8, 0x57];
    code.extend([0x60, 17, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    code.resize(CHUNK, 0);
    code.extend([0x5b, 0x60, 99, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    for (budget, marker, indices, spent) in
        [(60_000, 99, vec![0, 1], 57_360 + 21 + 1 + 18), (100_000, 17, vec![0], 28_680 + 21 + 18)]
    {
        let expected = resident(&code, budget);
        assert_eq!(expected.0, InstrStop::Return);
        assert_eq!(expected.1.len(), 32);
        assert_eq!(expected.1[31], marker);
        assert_eq!(expected.2, spent);
        let actual = execute(code.clone(), true, budget);
        assert_eq!((actual.0, actual.1, actual.2), expected);
        assert_eq!(actual.3, indices);
        assert_eq!(actual.4, usize::from(code.len() <= CHUNK));
    }
}

#[test]
fn tip1143_t34_fixed_resident_state_log_and_rollback_vectors() {
    // Independent flat-program vector: one cold zero->42 SSTORE, one
    // 32-byte LOG1, then RETURN/REVERT. No production gas planner is used.
    // Prague ordinary gas: entry jump 11, JUMPDEST 1, nine PUSH1s 27,
    // SSTORE 22100, MSTORE 3, memory expansion 3, LOG1 1006 = 23151.
    let ordinary = 11 + 1 + 9 * 3 + 22_100 + 3 + 3 + 1006;
    assert_eq!(ordinary, 23_151);
    for terminator in [0xf3, 0xfd] {
        let mut code = vec![0x61, (CHUNK >> 8) as u8, CHUNK as u8, 0x56];
        code.resize(CHUNK, 0);
        code.extend([
            0x5b, 0x60, 42, 0x60, 0, 0x55, 0x60, 42, 0x60, 0, 0x52, 0x60, 7, 0x60, 32, 0x60, 0,
            0xa1, 0x60, 32, 0x60, 0, terminator,
        ]);
        let total = 2 * 28680 + ordinary;
        for budget in [total - 1, total, total + 1] {
            let provider = Provider {
                hash: keccak256(&code),
                code: Bytes::copy_from_slice(&code),
                reads: Rc::default(),
                full_reads: Rc::default(),
            };
            let reads = provider.reads.clone();
            let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
                ExecutionConfig::for_spec_and_version(
                    SpecId::PRAGUE,
                    Version::new(SpecId::PRAGUE).with_tip1143(true),
                ),
                SpecId::PRAGUE,
                BlockEnvExt::default(),
                TxRegistry::new(),
                Db::new(provider),
                Precompiles::base(SpecId::PRAGUE),
            );
            for _ in 0..2 {
                let mut gas = GasTracker::new(budget);
                let frame = prepare_initial_frame(
                    &mut evm,
                    Address::repeat_byte(0x55),
                    0,
                    TxKind::Call(Address::repeat_byte(0x44)),
                    &Bytes::new(),
                    U256::ZERO,
                    &mut gas,
                )
                .unwrap();
                let result = execute_initial_frame(
                    &mut evm,
                    &TxEnvExt::default(),
                    frame,
                    &mut gas,
                    budget,
                    0,
                )
                .unwrap();
                let success = budget >= total && terminator == 0xf3;
                assert_eq!(
                    result.stop,
                    if budget < total {
                        InstrStop::OutOfGas
                    } else if success {
                        InstrStop::Return
                    } else {
                        InstrStop::Revert
                    }
                );
                assert_eq!(gas.spent(), total.min(budget));
                assert_eq!(gas.refunded(), 0);
                if budget >= total {
                    assert_eq!(result.output.as_ref(), U256::from(42).to_be_bytes::<32>());
                } else {
                    assert!(result.output.is_empty());
                }
                let logs = evm.state_mut().logs();
                assert_eq!(logs.len(), usize::from(success));
                if success {
                    assert_eq!(logs[0].address, Address::repeat_byte(0x44));
                    assert_eq!(
                        logs[0].data.topics(),
                        &[B256::from(U256::from(7).to_be_bytes::<32>())]
                    );
                    assert_eq!(logs[0].data.data.as_ref(), U256::from(42).to_be_bytes::<32>());
                }
                let mut account = evm.state_mut().account(&Address::repeat_byte(0x44)).unwrap();
                assert_eq!(
                    account.storage().into_slot(U256::ZERO).unwrap().current(),
                    if success { U256::from(42) } else { U256::ZERO }
                );
                assert_eq!(*reads.borrow(), [0, 1]);
                drop(account);
                // Reset consensus effects while retaining the production byte cache.
                evm.state_mut().clear_transaction_state();
            }
        }
    }
}

#[derive(Clone, Debug)]
struct CallProvider {
    parent: Bytes,
    child: Provider,
    parent_reads: Rc<RefCell<usize>>,
}

impl Database for CallProvider {
    type Error = io::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        if *address == Address::repeat_byte(0x66) {
            Ok(Some(AccountInfo {
                nonce: 1,
                code_hash: keccak256(&self.parent),
                ..Default::default()
            }))
        } else {
            self.child.get_account(address)
        }
    }

    fn get_code_by_hash(&mut self, _: &B256) -> Result<Bytecode, Self::Error> {
        *self.parent_reads.borrow_mut() += 1;
        Ok(Bytecode::new_legacy(self.parent.clone()))
    }

    fn get_storage(&mut self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    fn get_block_hash(&mut self, _: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }

    fn get_code_chunk_by_hash(
        &mut self,
        hash: &B256,
        index: u32,
    ) -> Result<Option<CodeChunk>, Self::Error> {
        if *hash == keccak256(&self.parent) {
            assert_eq!(index, 0);
            *self.parent_reads.borrow_mut() += 1;
            Ok(Some(CodeChunk::new(self.parent.clone())))
        } else {
            self.child.get_code_chunk_by_hash(hash, index)
        }
    }
}

#[test]
fn tip1143_t34_resident_call_driver_compares_forwarding_child_oog_and_revert() {
    // Bounded driver implements exactly one non-value CALL, no input, 64-byte
    // output, followed by success-word storage and RETURN. Account and chunk
    // tariffs are charged before EIP-150; child execution uses flat bytes.
    for terminator in [0xf3, 0xfd] {
        let mut child = vec![0x5a, 0x60, 0, 0x52, 0x61, (CHUNK >> 8) as u8, CHUNK as u8, 0x56];
        child.resize(CHUNK, 0);
        child.extend([0x5b, 0x5a, 0x60, 32, 0x52, 0x60, 64, 0x60, 0, terminator]);
        for requested in [10_000u64, 40_000, 1_000_000] {
            for budget in [150_000u64, 150_001, 150_063, 150_064] {
                let mut parent = vec![0x60, 64, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73];
                parent.extend_from_slice(Address::repeat_byte(0x44).as_slice());
                parent.extend([
                    0x62,
                    (requested >> 16) as u8,
                    (requested >> 8) as u8,
                    requested as u8,
                    0xf1,
                ]);
                parent.extend([0x60, 64, 0x52, 0x60, 96, 0x60, 0, 0xf3]);
                let provider = CallProvider {
                    parent: parent.into(),
                    child: Provider {
                        hash: keccak256(&child),
                        code: Bytes::copy_from_slice(&child),
                        reads: Rc::default(),
                        full_reads: Rc::default(),
                    },
                    parent_reads: Rc::default(),
                };
                let reads = provider.child.reads.clone();
                let parent_reads = provider.parent_reads.clone();
                let mut evm = Evm::<'_, BaseEvmTypes>::new_with_execution_config(
                    ExecutionConfig::for_spec_and_version(
                        SpecId::PRAGUE,
                        Version::new(SpecId::PRAGUE).with_tip1143(true),
                    ),
                    SpecId::PRAGUE,
                    BlockEnvExt::default(),
                    TxRegistry::new(),
                    Db::new(provider),
                    Precompiles::base(SpecId::PRAGUE),
                );
                let before_child = 28680 + 21 + 6 + 2600 + 28680;
                let available = budget - before_child;
                let forwarded = requested.min(available - available / 64);
                let expected_child = resident_frame(&child, forwarded, false);
                let success = expected_child.0 == InstrStop::Return;
                let mut expected_output = vec![0; 96];
                if matches!(expected_child.0, InstrStop::Return | InstrStop::Revert) {
                    assert_eq!(expected_child.1.len(), 64);
                    expected_output[..64].copy_from_slice(&expected_child.1);
                    assert_eq!(
                        U256::from_be_slice(&expected_child.1[..32]),
                        U256::from(forwarded - 2)
                    );
                    // First GAS/MSTORE/expansion + jump consume 22; the remote
                    // JUMPDEST and GAS follow the cold destination tariff.
                    assert_eq!(
                        U256::from_be_slice(&expected_child.1[32..]),
                        U256::from(forwarded - 22 - 28680 - 1 - 2)
                    );
                } else {
                    assert_eq!(expected_child.0, InstrStop::OutOfGas);
                    assert_eq!(expected_child.2, forwarded);
                }
                expected_output[95] = u8::from(success);
                for round in 0..2 {
                    let mut gas = GasTracker::new(budget);
                    let frame = prepare_initial_frame(
                        &mut evm,
                        Address::repeat_byte(0x55),
                        0,
                        TxKind::Call(Address::repeat_byte(0x66)),
                        &Bytes::new(),
                        U256::ZERO,
                        &mut gas,
                    )
                    .unwrap();
                    let result = execute_initial_frame(
                        &mut evm,
                        &TxEnvExt::default(),
                        frame,
                        &mut gas,
                        budget,
                        0,
                    )
                    .unwrap();
                    assert_eq!(result.stop, InstrStop::Return);
                    assert_eq!(
                        result.output.as_ref(),
                        expected_output,
                        "requested={requested}, round={round}"
                    );
                    assert_eq!(gas.spent(), before_child + expected_child.2 + 15);
                    assert_eq!(gas.refunded(), 0);
                    assert!(evm.state_mut().logs().is_empty());
                    assert_eq!(*parent_reads.borrow(), 1);
                    assert_eq!(
                        *reads.borrow(),
                        if requested == 10_000 { vec![0] } else { vec![0, 1] }
                    );
                    evm.state_mut().clear_transaction_state();
                }
            }
        }
    }
}

/// Calibrate the bounded oracle against ordinary instruction equations before
/// using it as the same-rules reference for sparse execution.
#[test]
fn tip1143_t34_ordinary_oracle_calibration_vectors() {
    let budget = 100_000;
    let gas = vec![0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
    let pc = vec![0x60, 4, 0x56, 0, 0x5b, 0x58, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];
    // GAS: 2 + three PUSHes 9 + MSTORE 3 + memory 3 = 17.
    // PC: PUSH/JUMP/JUMPDEST 12 + PC 2 + the same tail 15 = 29.
    for (code, word, spent) in [(gas, budget - 2, 17), (pc, 5, 29)] {
        let result = resident_frame(&code, budget, false);
        assert_eq!(result.0, InstrStop::Return);
        assert_eq!(result.1.as_ref(), U256::from(word).to_be_bytes::<32>());
        assert_eq!(result.2, spent);
        let enabled = resident(&code, budget);
        assert_eq!(enabled.0, InstrStop::Return);
        assert_eq!(enabled.2, 28680 + spent);
        assert_eq!(
            enabled.1.as_ref(),
            U256::from(if spent == 17 { word - 28680 } else { word }).to_be_bytes::<32>()
        );
    }
}
