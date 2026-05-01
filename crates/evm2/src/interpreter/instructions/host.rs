use super::utils::{address_to_word, b256_to_word, check_spec};
use crate::interpreter::{InstrStop, SpecId, Word};
use evm2_macros::instruction;

const BLOCK_HASH_HISTORY: u64 = 256;

#[instruction]
pub(in crate::interpreter) fn balance(cx: _, [addr]: [Word]) -> out {
    *out = cx.state.host.balance(*addr);
}

#[instruction]
pub(in crate::interpreter) fn blockhash(cx: _, [number]: [Word]) -> Result<out> {
    *out = if let Some(diff) = cx.state.block.number.checked_sub(*number) {
        let diff = u64::try_from(diff).unwrap_or(u64::MAX);
        if diff == 0 || diff > BLOCK_HASH_HISTORY {
            Word::ZERO
        } else {
            let number = u64::try_from(*number).unwrap_or(u64::MAX);
            cx.state
                .host
                .block_hash(number)
                .map(b256_to_word)
                .ok_or(InstrStop::FatalExternalError)?
        }
    } else {
        Word::ZERO
    };
}

#[instruction]
pub(in crate::interpreter) fn coinbase(cx: _) -> out {
    *out = address_to_word(cx.state.block.beneficiary);
}

#[instruction]
pub(in crate::interpreter) fn timestamp(cx: _) -> out {
    *out = cx.state.block.timestamp;
}

#[instruction]
pub(in crate::interpreter) fn block_number(cx: _) -> out {
    *out = cx.state.block.number;
}

#[instruction]
pub(in crate::interpreter) fn difficulty(cx: _) -> out {
    *out = if cx.state.spec.enables(SpecId::MERGE) {
        cx.state.block.prevrandao.map(b256_to_word).unwrap()
    } else {
        cx.state.block.difficulty
    };
}

#[instruction]
pub(in crate::interpreter) fn gaslimit(cx: _) -> out {
    *out = Word::from(cx.state.block.gas_limit);
}

#[instruction]
pub(in crate::interpreter) fn selfbalance(cx: _) -> out {
    *out = cx.state.host.balance(address_to_word(cx.state.tx.address));
}

#[instruction]
pub(in crate::interpreter) fn basefee(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::LONDON)?;
    *out = Word::from(cx.state.block.basefee);
}

#[instruction]
pub(in crate::interpreter) fn blobbasefee(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::CANCUN)?;
    *out = Word::from(cx.state.block.blob_basefee);
}

#[instruction]
pub(in crate::interpreter) fn slotnum(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::AMSTERDAM)?;
    *out = Word::from(cx.state.block.slot_num);
}

#[cfg(test)]
mod tests {
    use crate::{
        env::{BlockEnv, TxEnv},
        interpreter::{
            InstrStop, SpecId, Word,
            instructions::{
                tests::{TestHost, assert_stack, push, run_with_host, run_with_host_and_spec},
                utils::{address_to_word, b256_to_word},
            },
            op,
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256};

    fn neg(value: u64) -> Word {
        Word::from(0).wrapping_sub(Word::from(value))
    }

    fn test_host(block: BlockEnv) -> TestHost {
        TestHost { block, ..TestHost::default() }
    }

    #[test]
    fn balance_opcode() {
        assert_stack!(BALANCE(0xbeef), 0xbeef);
        assert_stack!(BALANCE(0), 0);
        assert_stack!(BALANCE(neg(1)), neg(1));
    }

    #[test]
    fn blockhash_opcode() {
        let mut host = test_host(BlockEnv { number: Word::from(10), ..BlockEnv::default() });
        let mut code = Vec::new();
        push(&mut code, 9);
        code.push(op::BLOCKHASH);
        code.push(op::STOP);

        let interpreter = run_with_host(code, &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [b256_to_word(B256::with_last_byte(9))]);

        let mut code = Vec::new();
        push(&mut code, 10);
        code.push(op::BLOCKHASH);
        code.push(op::STOP);
        let interpreter = run_with_host(code, &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn coinbase_opcode() {
        let beneficiary = Address::from([0x44; 20]);
        let mut host = test_host(BlockEnv { beneficiary, ..BlockEnv::default() });
        let interpreter = run_with_host([op::COINBASE, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [address_to_word(beneficiary)]);
    }

    #[test]
    fn timestamp_opcode() {
        let mut host = test_host(BlockEnv { timestamp: Word::from(12), ..BlockEnv::default() });
        let interpreter = run_with_host([op::TIMESTAMP, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(12)]);
    }

    #[test]
    fn number_opcode() {
        let mut host = test_host(BlockEnv { number: Word::from(13), ..BlockEnv::default() });
        let interpreter = run_with_host([op::NUMBER, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(13)]);
    }

    #[test]
    fn difficulty_opcode() {
        let randao = B256::with_last_byte(0x55);
        let mut host = test_host(BlockEnv {
            difficulty: Word::from(14),
            prevrandao: Some(randao),
            ..BlockEnv::default()
        });
        let interpreter = run_with_host([op::DIFFICULTY, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(14)]);

        let interpreter =
            run_with_host_and_spec([op::DIFFICULTY, op::STOP], &mut host, SpecId::MERGE);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [b256_to_word(randao)]);
    }

    #[test]
    fn gaslimit_opcode() {
        let mut host = test_host(BlockEnv { gas_limit: 15, ..BlockEnv::default() });
        let interpreter = run_with_host([op::GASLIMIT, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(15)]);
    }

    #[test]
    fn selfbalance_opcode() {
        let address = Address::from([0x66; 20]);
        let mut host =
            TestHost { tx: TxEnv { address, ..TxEnv::default() }, ..TestHost::default() };
        let interpreter = run_with_host([op::SELFBALANCE, op::STOP], &mut host);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [address_to_word(address)]);
    }

    #[test]
    fn basefee_opcode() {
        let mut host = test_host(BlockEnv { basefee: 16, ..BlockEnv::default() });
        let interpreter =
            run_with_host_and_spec([op::BASEFEE, op::STOP], &mut host, SpecId::LONDON);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(16)]);
    }

    #[test]
    fn blobbasefee_opcode() {
        let mut host = test_host(BlockEnv { blob_basefee: 17, ..BlockEnv::default() });
        let interpreter =
            run_with_host_and_spec([op::BLOBBASEFEE, op::STOP], &mut host, SpecId::CANCUN);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(17)]);
    }

    #[test]
    fn slotnum_opcode() {
        let mut host = test_host(BlockEnv { slot_num: 18, ..BlockEnv::default() });
        let interpreter =
            run_with_host_and_spec([op::SLOTNUM, op::STOP], &mut host, SpecId::AMSTERDAM);
        assert!(matches!(interpreter.err, InstrStop::Stop));
        assert_eq!(interpreter.stack(), [Word::from(18)]);
    }
}
