use super::utils::{address_to_word, as_usize_saturated, b256_to_word, check_spec};
use crate::interpreter::{Host, InstrStop, SpecId, Word};
use evm2_macros::instruction;

const BLOCK_HASH_HISTORY: u64 = 256;

#[instruction]
pub(in crate::interpreter) fn blockhash(cx: _, [number]: [Word]) -> Result<out> {
    *out = if let Some(diff) = cx.state.host.block_env().number.checked_sub(number) {
        let diff = u64::try_from(diff).unwrap_or(u64::MAX);
        if diff == 0 || diff > BLOCK_HASH_HISTORY {
            Word::ZERO
        } else {
            let number = u64::try_from(number).unwrap_or(u64::MAX);
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
    *out = address_to_word(cx.state.host.block_env().beneficiary);
}

#[instruction]
pub(in crate::interpreter) fn timestamp(cx: _) -> out {
    *out = cx.state.host.block_env().timestamp;
}

#[instruction]
pub(in crate::interpreter) fn block_number(cx: _) -> out {
    *out = cx.state.host.block_env().number;
}

#[instruction]
pub(in crate::interpreter) fn difficulty(cx: _) -> out {
    *out = if cx.state.spec.enables(SpecId::MERGE) {
        cx.state.host.block_env().prevrandao
    } else {
        cx.state.host.block_env().difficulty
    };
}

#[instruction]
pub(in crate::interpreter) fn gaslimit(cx: _) -> out {
    *out = cx.state.host.block_env().gas_limit;
}

#[instruction]
pub(in crate::interpreter) fn chainid(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::ISTANBUL)?;
    *out = cx.state.tx.chain_id;
}

#[instruction]
pub(in crate::interpreter) fn selfbalance(cx: _) -> Result<out> {
    *out = cx
        .state
        .host
        .load_account(address_to_word(cx.state.message.destination), false, false)?
        .balance;
}

#[instruction]
pub(in crate::interpreter) fn basefee(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::LONDON)?;
    *out = cx.state.host.block_env().basefee;
}

#[instruction]
pub(in crate::interpreter) fn blobhash(cx: _, [index]: [Word]) -> Result<out> {
    check_spec(cx.state.spec, SpecId::CANCUN)?;
    let index = as_usize_saturated(index);
    *out = cx.state.tx.blob_hashes.get(index).copied().unwrap_or_default();
}

#[instruction]
pub(in crate::interpreter) fn blobbasefee(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::CANCUN)?;
    *out = cx.state.host.block_env().blob_basefee;
}

#[instruction]
pub(in crate::interpreter) fn slotnum(cx: _) -> Result<out> {
    check_spec(cx.state.spec, SpecId::AMSTERDAM)?;
    *out = cx.state.host.block_env().slot_num;
}

#[cfg(test)]
mod tests {
    use crate::{
        env::{BlockEnv, TxEnv},
        interpreter::{
            InstrStop, SpecId, Word,
            instructions::{
                tests::{RunConfig, TestHost, push, run},
                utils::{address_to_word, b256_to_word},
            },
            op,
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256};

    fn test_host(block: BlockEnv) -> TestHost {
        TestHost { block, ..TestHost::default() }
    }

    #[test]
    fn blockhash_opcode() {
        let mut host = test_host(BlockEnv { number: Word::from(10), ..BlockEnv::default() });
        let mut code = Vec::new();
        push(&mut code, 9);
        code.push(op::BLOCKHASH);
        code.push(op::STOP);

        let interpreter = run(RunConfig::new(code).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(B256::with_last_byte(9))]);

        let mut code = Vec::new();
        push(&mut code, 10);
        code.push(op::BLOCKHASH);
        code.push(op::STOP);
        let interpreter = run(RunConfig::new(code).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn coinbase_opcode() {
        let beneficiary = Address::from([0x44; 20]);
        let mut host = test_host(BlockEnv { beneficiary, ..BlockEnv::default() });
        let interpreter = run(RunConfig::new([op::COINBASE, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(beneficiary)]);
    }

    #[test]
    fn timestamp_opcode() {
        let mut host = test_host(BlockEnv { timestamp: Word::from(12), ..BlockEnv::default() });
        let interpreter = run(RunConfig::new([op::TIMESTAMP, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(12)]);
    }

    #[test]
    fn number_opcode() {
        let mut host = test_host(BlockEnv { number: Word::from(13), ..BlockEnv::default() });
        let interpreter = run(RunConfig::new([op::NUMBER, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(13)]);
    }

    #[test]
    fn difficulty_opcode() {
        let randao = B256::with_last_byte(0x55);
        let mut host = test_host(BlockEnv {
            difficulty: Word::from(14),
            prevrandao: b256_to_word(randao),
            ..BlockEnv::default()
        });
        let interpreter = run(RunConfig::new([op::DIFFICULTY, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(14)]);

        let interpreter =
            run(RunConfig::new([op::DIFFICULTY, op::STOP]).host(&mut host).spec(SpecId::MERGE));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(randao)]);
    }

    #[test]
    fn gaslimit_opcode() {
        let mut host = test_host(BlockEnv { gas_limit: Word::from(15), ..BlockEnv::default() });
        let interpreter = run(RunConfig::new([op::GASLIMIT, op::STOP]).host(&mut host));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(15)]);
    }

    #[test]
    fn chainid_opcode() {
        let mut host = TestHost::default();
        let tx_env = TxEnv { chain_id: Word::from(1), ..TxEnv::default() };
        let interpreter = run(RunConfig::new([op::CHAINID, op::STOP])
            .host(&mut host)
            .tx_env(tx_env)
            .spec(SpecId::ISTANBUL));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(1)]);
    }

    #[test]
    fn selfbalance_opcode() {
        let address = Address::from([0x66; 20]);
        let mut host = TestHost::default();
        let message = crate::interpreter::Message {
            destination: address,
            gas_limit: 10_000,
            ..Default::default()
        };
        let interpreter =
            run(RunConfig::new([op::SELFBALANCE, op::STOP]).host(&mut host).message(message));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [address_to_word(address)]);
    }

    #[test]
    fn basefee_opcode() {
        let mut host = test_host(BlockEnv { basefee: Word::from(16), ..BlockEnv::default() });
        let interpreter =
            run(RunConfig::new([op::BASEFEE, op::STOP]).host(&mut host).spec(SpecId::LONDON));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(16)]);
    }

    #[test]
    fn blobhash_opcode() {
        let hash = B256::with_last_byte(0x42);
        let mut host = TestHost::default();
        let tx_env = TxEnv { blob_hashes: Vec::from([b256_to_word(hash)]), ..TxEnv::default() };

        let interpreter = run(RunConfig::new([op::PUSH0, op::BLOBHASH, op::STOP])
            .host(&mut host)
            .tx_env(tx_env.clone())
            .spec(SpecId::CANCUN));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [b256_to_word(hash)]);

        let interpreter = run(RunConfig::new([op::PUSH1, 0x01, op::BLOBHASH, op::STOP])
            .host(&mut host)
            .tx_env(tx_env)
            .spec(SpecId::CANCUN));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [0]);
    }

    #[test]
    fn blobbasefee_opcode() {
        let mut host = test_host(BlockEnv { blob_basefee: Word::from(17), ..BlockEnv::default() });
        let interpreter =
            run(RunConfig::new([op::BLOBBASEFEE, op::STOP]).host(&mut host).spec(SpecId::CANCUN));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(17)]);
    }

    #[test]
    fn slotnum_opcode() {
        let mut host = test_host(BlockEnv { slot_num: Word::from(18), ..BlockEnv::default() });
        let interpreter =
            run(RunConfig::new([op::SLOTNUM, op::STOP]).host(&mut host).spec(SpecId::AMSTERDAM));
        core::assert_matches!(interpreter.err, InstrStop::Stop);
        assert_eq!(interpreter.stack(), [Word::from(18)]);
    }
}
