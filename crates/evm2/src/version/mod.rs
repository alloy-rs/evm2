//! EVM version data.

use crate::{
    EvmConfig, EvmTypes,
    interpreter::{SpecId, opcode::op},
};
use core::marker::PhantomData;

mod gas_params;
pub use gas_params::{GasId, GasParams, num_words};

mod static_gas_table;
pub use static_gas_table::StaticGasTable;

mod instruction_impl_table;
pub use instruction_impl_table::InstructionImplTable;

/// EVM version data.
#[derive(Debug)]
pub struct EvmVersion<T: EvmTypes = crate::BaseEvmTypes> {
    /// Active hard fork specification.
    pub spec_id: SpecId,
    /// Static opcode gas table.
    pub static_gas_table: StaticGasTable,
    /// Dynamic gas parameter table.
    pub gas_params: GasParams,
    /// Instruction implementations.
    pub instruction_impls: InstructionImplTable<T>,
}

struct VersionEvmConfig<T: EvmTypes, const SPEC_ID: u8>(PhantomData<fn() -> T>);

impl<T: EvmTypes, const SPEC_ID: u8> EvmTypes for VersionEvmConfig<T, SPEC_ID> {
    type Tx = T::Tx;
    type Host = T::Host;
    type Database = T::Database;
    type Precompiles = T::Precompiles;
}

impl<T: EvmTypes, const SPEC_ID: u8> EvmConfig for VersionEvmConfig<T, SPEC_ID> {
    const VERSION: &'static EvmVersion<Self> =
        &EvmVersion::new_base_without_instructions(match SpecId::try_from_u8(SPEC_ID) {
            Some(spec_id) => spec_id,
            None => panic!("invalid EVM specification ID"),
        });
}

impl<T: EvmTypes> EvmVersion<T> {
    /// Creates the base EVM version for `spec`.
    pub const fn new_base(spec_id: SpecId) -> Self {
        Self::new_base_inner::<true>(spec_id)
    }

    const fn new_base_without_instructions(spec_id: SpecId) -> Self {
        Self::new_base_inner::<false>(spec_id)
    }

    const fn new_base_inner<const INSTRUCTIONS: bool>(spec_id: SpecId) -> Self {
        use crate::interpreter::{gas::*, instructions as instr};
        use GasId::*;

        let mut gt = StaticGasTable::empty();
        let mut gp = GasParams::empty();
        let mut i = InstructionImplTable::empty();

        macro_rules! new_op {
            ($spec:expr, $name:ident, $cost:expr, $instr:path) => {
                gt.set(op::$name, $cost as u16);
                if INSTRUCTIONS {
                    i.set(
                        op::$name,
                        Some(
                            <$instr as instr::table::Instruction<T>>::execute::<
                                VersionEvmConfig<T, $spec>,
                            >,
                        ),
                    );
                }
            };
        }

        if spec_id.enables(SpecId::FRONTIER) {
            const C: u8 = SpecId::FRONTIER as u8;

            new_op!(C, STOP, ZERO, instr::stop<T>);
            new_op!(C, ADD, VERYLOW, instr::add<T>);
            new_op!(C, MUL, LOW, instr::mul<T>);
            new_op!(C, SUB, VERYLOW, instr::sub<T>);
            new_op!(C, DIV, LOW, instr::div<T>);
            new_op!(C, SDIV, LOW, instr::sdiv<T>);
            new_op!(C, MOD, LOW, instr::rem<T>);
            new_op!(C, SMOD, LOW, instr::smod<T>);
            new_op!(C, ADDMOD, MID, instr::addmod<T>);
            new_op!(C, MULMOD, MID, instr::mulmod<T>);
            new_op!(C, EXP, EXP, instr::exp<T>);
            new_op!(C, SIGNEXTEND, LOW, instr::signextend<T>);
            new_op!(C, LT, VERYLOW, instr::lt<T>);
            new_op!(C, GT, VERYLOW, instr::gt<T>);
            new_op!(C, SLT, VERYLOW, instr::slt<T>);
            new_op!(C, SGT, VERYLOW, instr::sgt<T>);
            new_op!(C, EQ, VERYLOW, instr::eq<T>);
            new_op!(C, ISZERO, VERYLOW, instr::iszero<T>);
            new_op!(C, AND, VERYLOW, instr::bitand<T>);
            new_op!(C, OR, VERYLOW, instr::bitor<T>);
            new_op!(C, XOR, VERYLOW, instr::bitxor<T>);
            new_op!(C, NOT, VERYLOW, instr::not<T>);
            new_op!(C, BYTE, VERYLOW, instr::byte<T>);
            new_op!(C, KECCAK256, KECCAK256, instr::keccak256<T>);
            new_op!(C, ADDRESS, BASE, instr::address<T>);
            new_op!(C, BALANCE, 20, instr::balance<T>);
            new_op!(C, ORIGIN, BASE, instr::origin<T>);
            new_op!(C, CALLER, BASE, instr::caller<T>);
            new_op!(C, CALLVALUE, BASE, instr::callvalue<T>);
            new_op!(C, CALLDATALOAD, VERYLOW, instr::calldataload<T>);
            new_op!(C, CALLDATASIZE, BASE, instr::calldatasize<T>);
            new_op!(C, CALLDATACOPY, VERYLOW, instr::calldatacopy<T>);
            new_op!(C, CODESIZE, BASE, instr::codesize<T>);
            new_op!(C, CODECOPY, VERYLOW, instr::codecopy<T>);
            new_op!(C, GASPRICE, BASE, instr::gasprice<T>);
            new_op!(C, EXTCODESIZE, 20, instr::extcodesize<T>);
            new_op!(C, EXTCODECOPY, 20, instr::extcodecopy<T>);
            new_op!(C, BLOCKHASH, BLOCKHASH, instr::blockhash<T>);
            new_op!(C, COINBASE, BASE, instr::coinbase<T>);
            new_op!(C, TIMESTAMP, BASE, instr::timestamp<T>);
            new_op!(C, NUMBER, BASE, instr::block_number<T>);
            new_op!(C, DIFFICULTY, BASE, instr::difficulty<T>);
            new_op!(C, GASLIMIT, BASE, instr::gaslimit<T>);
            new_op!(C, POP, BASE, instr::pop<T>);
            new_op!(C, MLOAD, VERYLOW, instr::mload<T>);
            new_op!(C, MSTORE, VERYLOW, instr::mstore<T>);
            new_op!(C, MSTORE8, VERYLOW, instr::mstore8<T>);
            new_op!(C, SLOAD, 50, instr::sload<T>);
            new_op!(C, SSTORE, ZERO, instr::sstore<T>);
            new_op!(C, JUMP, MID, instr::jump<T>);
            new_op!(C, JUMPI, HIGH, instr::jumpi<T>);
            new_op!(C, PC, BASE, instr::pc<T>);
            new_op!(C, MSIZE, BASE, instr::msize<T>);
            new_op!(C, GAS, BASE, instr::gas<T>);
            new_op!(C, JUMPDEST, JUMPDEST, instr::jumpdest<T>);
            new_op!(C, PUSH1, VERYLOW, instr::push<T, 1>);
            new_op!(C, PUSH2, VERYLOW, instr::push<T, 2>);
            new_op!(C, PUSH3, VERYLOW, instr::push<T, 3>);
            new_op!(C, PUSH4, VERYLOW, instr::push<T, 4>);
            new_op!(C, PUSH5, VERYLOW, instr::push<T, 5>);
            new_op!(C, PUSH6, VERYLOW, instr::push<T, 6>);
            new_op!(C, PUSH7, VERYLOW, instr::push<T, 7>);
            new_op!(C, PUSH8, VERYLOW, instr::push<T, 8>);
            new_op!(C, PUSH9, VERYLOW, instr::push<T, 9>);
            new_op!(C, PUSH10, VERYLOW, instr::push<T, 10>);
            new_op!(C, PUSH11, VERYLOW, instr::push<T, 11>);
            new_op!(C, PUSH12, VERYLOW, instr::push<T, 12>);
            new_op!(C, PUSH13, VERYLOW, instr::push<T, 13>);
            new_op!(C, PUSH14, VERYLOW, instr::push<T, 14>);
            new_op!(C, PUSH15, VERYLOW, instr::push<T, 15>);
            new_op!(C, PUSH16, VERYLOW, instr::push<T, 16>);
            new_op!(C, PUSH17, VERYLOW, instr::push<T, 17>);
            new_op!(C, PUSH18, VERYLOW, instr::push<T, 18>);
            new_op!(C, PUSH19, VERYLOW, instr::push<T, 19>);
            new_op!(C, PUSH20, VERYLOW, instr::push<T, 20>);
            new_op!(C, PUSH21, VERYLOW, instr::push<T, 21>);
            new_op!(C, PUSH22, VERYLOW, instr::push<T, 22>);
            new_op!(C, PUSH23, VERYLOW, instr::push<T, 23>);
            new_op!(C, PUSH24, VERYLOW, instr::push<T, 24>);
            new_op!(C, PUSH25, VERYLOW, instr::push<T, 25>);
            new_op!(C, PUSH26, VERYLOW, instr::push<T, 26>);
            new_op!(C, PUSH27, VERYLOW, instr::push<T, 27>);
            new_op!(C, PUSH28, VERYLOW, instr::push<T, 28>);
            new_op!(C, PUSH29, VERYLOW, instr::push<T, 29>);
            new_op!(C, PUSH30, VERYLOW, instr::push<T, 30>);
            new_op!(C, PUSH31, VERYLOW, instr::push<T, 31>);
            new_op!(C, PUSH32, VERYLOW, instr::push<T, 32>);
            new_op!(C, DUP1, VERYLOW, instr::dup<T, 1>);
            new_op!(C, DUP2, VERYLOW, instr::dup<T, 2>);
            new_op!(C, DUP3, VERYLOW, instr::dup<T, 3>);
            new_op!(C, DUP4, VERYLOW, instr::dup<T, 4>);
            new_op!(C, DUP5, VERYLOW, instr::dup<T, 5>);
            new_op!(C, DUP6, VERYLOW, instr::dup<T, 6>);
            new_op!(C, DUP7, VERYLOW, instr::dup<T, 7>);
            new_op!(C, DUP8, VERYLOW, instr::dup<T, 8>);
            new_op!(C, DUP9, VERYLOW, instr::dup<T, 9>);
            new_op!(C, DUP10, VERYLOW, instr::dup<T, 10>);
            new_op!(C, DUP11, VERYLOW, instr::dup<T, 11>);
            new_op!(C, DUP12, VERYLOW, instr::dup<T, 12>);
            new_op!(C, DUP13, VERYLOW, instr::dup<T, 13>);
            new_op!(C, DUP14, VERYLOW, instr::dup<T, 14>);
            new_op!(C, DUP15, VERYLOW, instr::dup<T, 15>);
            new_op!(C, DUP16, VERYLOW, instr::dup<T, 16>);
            new_op!(C, SWAP1, VERYLOW, instr::swap<T, 1>);
            new_op!(C, SWAP2, VERYLOW, instr::swap<T, 2>);
            new_op!(C, SWAP3, VERYLOW, instr::swap<T, 3>);
            new_op!(C, SWAP4, VERYLOW, instr::swap<T, 4>);
            new_op!(C, SWAP5, VERYLOW, instr::swap<T, 5>);
            new_op!(C, SWAP6, VERYLOW, instr::swap<T, 6>);
            new_op!(C, SWAP7, VERYLOW, instr::swap<T, 7>);
            new_op!(C, SWAP8, VERYLOW, instr::swap<T, 8>);
            new_op!(C, SWAP9, VERYLOW, instr::swap<T, 9>);
            new_op!(C, SWAP10, VERYLOW, instr::swap<T, 10>);
            new_op!(C, SWAP11, VERYLOW, instr::swap<T, 11>);
            new_op!(C, SWAP12, VERYLOW, instr::swap<T, 12>);
            new_op!(C, SWAP13, VERYLOW, instr::swap<T, 13>);
            new_op!(C, SWAP14, VERYLOW, instr::swap<T, 14>);
            new_op!(C, SWAP15, VERYLOW, instr::swap<T, 15>);
            new_op!(C, SWAP16, VERYLOW, instr::swap<T, 16>);
            new_op!(C, LOG0, LOG, instr::log<T, 0>);
            new_op!(C, LOG1, LOG, instr::log<T, 1>);
            new_op!(C, LOG2, LOG, instr::log<T, 2>);
            new_op!(C, LOG3, LOG, instr::log<T, 3>);
            new_op!(C, LOG4, LOG, instr::log<T, 4>);
            new_op!(C, CREATE, ZERO, instr::create<T, false>);
            new_op!(C, CALL, 40, instr::call<T>);
            new_op!(C, CALLCODE, 40, instr::callcode<T>);
            new_op!(C, RETURN, ZERO, instr::r#return<T>);
            new_op!(C, INVALID, ZERO, instr::invalid<T>);
            new_op!(C, SELFDESTRUCT, ZERO, instr::selfdestruct<T>);

            gp.set(ExpByteGas, 10);
            gp.set(Logdata, LOGDATA);
            gp.set(Logtopic, LOGTOPIC);
            gp.set(CopyPerWord, COPY);
            gp.set(ExtcodecopyPerWord, COPY);
            gp.set(McopyPerWord, COPY);
            gp.set(Keccak256PerWord, KECCAK256WORD);
            gp.set(MemoryLinearCost, MEMORY);
            gp.set(MemoryQuadraticReduction, 512);
            gp.set(InitcodePerWord, INITCODE_WORD_COST);
            gp.set(Create, CREATE);
            gp.set(CallStipendReduction, 64);
            gp.set(TransferValueCost, CALLVALUE);
            gp.set(NewAccountCost, NEWACCOUNT);
            gp.set(SstoreStatic, SSTORE_RESET);
            gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - SSTORE_RESET);
            gp.set(SstoreSetRefund, SSTORE_SET - SSTORE_RESET);
            gp.set(SstoreClearingSlotRefund, REFUND_SSTORE_CLEARS);
            gp.set(SelfdestructRefund, SELFDESTRUCT_REFUND);
            gp.set(CallStipend, CALL_STIPEND);
            gp.set(CodeDepositCost, CODEDEPOSIT);
            gp.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER);
            gp.set(TxTokenCost, STANDARD_TOKEN_COST);
            gp.set(TxBaseStipend, 21000);
        }

        if spec_id.enables(SpecId::HOMESTEAD) {
            const C: u8 = SpecId::HOMESTEAD as u8;

            new_op!(C, DELEGATECALL, 40, instr::delegatecall<T>);

            gp.set(TxCreateCost, CREATE);
        }

        if spec_id.enables(SpecId::TANGERINE) {
            const C: u8 = SpecId::TANGERINE as u8;

            gt.set(op::SLOAD, 200);
            gt.set(op::BALANCE, 400);
            gt.set(op::EXTCODESIZE, 700);
            gt.set(op::EXTCODECOPY, 700);
            gt.set(op::CALL, 700);
            gt.set(op::CALLCODE, 700);
            gt.set(op::DELEGATECALL, 700);
            gt.set(op::SELFDESTRUCT, 5000);
            gp.set(NewAccountCostForSelfdestruct, NEWACCOUNT);

            new_op!(C, CREATE, ZERO, instr::create<T, false>);
            new_op!(C, CALL, 700, instr::call<T>);
            new_op!(C, CALLCODE, 700, instr::callcode<T>);
            new_op!(C, DELEGATECALL, 700, instr::delegatecall<T>);
            new_op!(C, SELFDESTRUCT, 5000, instr::selfdestruct<T>);
        }

        if spec_id.enables(SpecId::SPURIOUS_DRAGON) {
            const C: u8 = SpecId::SPURIOUS_DRAGON as u8;

            gp.set(ExpByteGas, 50);

            new_op!(C, EXP, EXP, instr::exp<T>);
        }

        if spec_id.enables(SpecId::BYZANTIUM) {
            const C: u8 = SpecId::BYZANTIUM as u8;

            new_op!(C, RETURNDATASIZE, BASE, instr::returndatasize<T>);
            new_op!(C, RETURNDATACOPY, VERYLOW, instr::returndatacopy<T>);
            new_op!(C, STATICCALL, 700, instr::staticcall<T>);
            new_op!(C, REVERT, ZERO, instr::revert<T>);
        }

        if spec_id.enables(SpecId::CONSTANTINOPLE) {
            const C: u8 = SpecId::CONSTANTINOPLE as u8;

            new_op!(C, SHL, VERYLOW, instr::shl<T>);
            new_op!(C, SHR, VERYLOW, instr::shr<T>);
            new_op!(C, SAR, VERYLOW, instr::sar<T>);
            new_op!(C, EXTCODEHASH, 400, instr::extcodehash<T>);
        }

        if spec_id.enables(SpecId::PETERSBURG) {
            const C: u8 = SpecId::PETERSBURG as u8;

            new_op!(C, CREATE2, ZERO, instr::create<T, true>);
        }

        if spec_id.enables(SpecId::ISTANBUL) {
            const C: u8 = SpecId::ISTANBUL as u8;

            new_op!(C, CHAINID, BASE, instr::chainid<T>);
            new_op!(C, SELFBALANCE, LOW, instr::selfbalance<T>);

            gt.set(op::SLOAD, ISTANBUL_SLOAD_GAS as u16);
            gt.set(op::BALANCE, 700);
            gt.set(op::EXTCODEHASH, 700);

            gp.set(SstoreStatic, ISTANBUL_SLOAD_GAS);
            gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
            gp.set(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
            gp.set(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);

            new_op!(C, SSTORE, ZERO, instr::sstore<T>);
        }

        if spec_id.enables(SpecId::BERLIN) {
            const C: u8 = SpecId::BERLIN as u8;

            gt.set(op::SLOAD, WARM_STORAGE_READ_COST as u16);
            gt.set(op::BALANCE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::EXTCODESIZE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::EXTCODEHASH, WARM_STORAGE_READ_COST as u16);
            gt.set(op::EXTCODECOPY, WARM_STORAGE_READ_COST as u16);
            gt.set(op::CALL, WARM_STORAGE_READ_COST as u16);
            gt.set(op::CALLCODE, WARM_STORAGE_READ_COST as u16);
            gt.set(op::DELEGATECALL, WARM_STORAGE_READ_COST as u16);
            gt.set(op::STATICCALL, WARM_STORAGE_READ_COST as u16);

            gp.set(SstoreStatic, WARM_STORAGE_READ_COST);
            gp.set(ColdAccountAdditionalCost, COLD_ACCOUNT_ACCESS_COST_ADDITIONAL);
            gp.set(ColdStorageAdditionalCost, COLD_SLOAD_COST - WARM_STORAGE_READ_COST);
            gp.set(ColdStorageCost, COLD_SLOAD_COST);
            gp.set(WarmStorageReadCost, WARM_STORAGE_READ_COST);
            gp.set(SstoreResetWithoutColdLoadCost, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
            gp.set(SstoreSetWithoutLoadCost, SSTORE_SET - WARM_STORAGE_READ_COST);
            gp.set(SstoreSetRefund, SSTORE_SET - WARM_STORAGE_READ_COST);
            gp.set(SstoreResetRefund, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
            gp.set(TxAccessListAddressCost, ACCESS_LIST_ADDRESS);
            gp.set(TxAccessListStorageKeyCost, ACCESS_LIST_STORAGE_KEY);

            new_op!(C, BALANCE, WARM_STORAGE_READ_COST, instr::balance<T>);
            new_op!(C, EXTCODESIZE, WARM_STORAGE_READ_COST, instr::extcodesize<T>);
            new_op!(C, EXTCODECOPY, WARM_STORAGE_READ_COST, instr::extcodecopy<T>);
            new_op!(C, EXTCODEHASH, WARM_STORAGE_READ_COST, instr::extcodehash<T>);
            new_op!(C, SLOAD, WARM_STORAGE_READ_COST, instr::sload<T>);
            new_op!(C, SSTORE, ZERO, instr::sstore<T>);
            new_op!(C, CALL, WARM_STORAGE_READ_COST, instr::call<T>);
            new_op!(C, CALLCODE, WARM_STORAGE_READ_COST, instr::callcode<T>);
            new_op!(C, DELEGATECALL, WARM_STORAGE_READ_COST, instr::delegatecall<T>);
            new_op!(C, STATICCALL, WARM_STORAGE_READ_COST, instr::staticcall<T>);
            new_op!(C, SELFDESTRUCT, 5000, instr::selfdestruct<T>);
        }

        if spec_id.enables(SpecId::LONDON) {
            const C: u8 = SpecId::LONDON as u8;

            new_op!(C, BASEFEE, BASE, instr::basefee<T>);

            gp.set(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
            gp.set(SelfdestructRefund, 0);

            new_op!(C, SSTORE, ZERO, instr::sstore<T>);
            new_op!(C, SELFDESTRUCT, 5000, instr::selfdestruct<T>);
        }

        if spec_id.enables(SpecId::SHANGHAI) {
            const C: u8 = SpecId::SHANGHAI as u8;

            new_op!(C, PUSH0, BASE, instr::push<T, 0>);

            gp.set(TxInitcodeCost, INITCODE_WORD_COST);

            new_op!(C, CREATE, ZERO, instr::create<T, false>);
            new_op!(C, CREATE2, ZERO, instr::create<T, true>);
        }

        if spec_id.enables(SpecId::CANCUN) {
            const C: u8 = SpecId::CANCUN as u8;

            new_op!(C, BLOBHASH, VERYLOW, instr::blobhash<T>);
            new_op!(C, BLOBBASEFEE, BASE, instr::blobbasefee<T>);
            new_op!(C, TLOAD, WARM_STORAGE_READ_COST, instr::tload<T>);
            new_op!(C, TSTORE, WARM_STORAGE_READ_COST, instr::tstore<T>);
            new_op!(C, MCOPY, VERYLOW, instr::mcopy<T>);
        }

        if spec_id.enables(SpecId::PRAGUE) {
            const C: u8 = SpecId::PRAGUE as u8;
            let _ = C;

            gp.set(TxEip7702PerEmptyAccountCost, EIP7702_PER_EMPTY_ACCOUNT_COST);
            gp.set(
                TxEip7702AuthRefund,
                EIP7702_PER_EMPTY_ACCOUNT_COST - EIP7702_PER_AUTH_BASE_COST,
            );
            gp.set(TxFloorCostPerToken, TOTAL_COST_FLOOR_PER_TOKEN);
            gp.set(TxFloorCostBaseGas, 21000);
        }

        if spec_id.enables(SpecId::OSAKA) {
            const C: u8 = SpecId::OSAKA as u8;

            new_op!(C, CLZ, LOW, instr::clz<T>);
            new_op!(C, DUPN, VERYLOW, instr::dupn<T>);
            new_op!(C, SWAPN, VERYLOW, instr::swapn<T>);
            new_op!(C, EXCHANGE, VERYLOW, instr::exchange<T>);
        }

        if spec_id.enables(SpecId::AMSTERDAM) {
            const C: u8 = SpecId::AMSTERDAM as u8;
            const CPSB: u32 = 1174;

            new_op!(C, SLOTNUM, BASE, instr::slotnum<T>);

            gp.set(Create, 9000);
            gp.set(TxCreateCost, 9000);
            gp.set(CodeDepositCost, 0);
            gp.set(NewAccountCost, 0);
            gp.set(NewAccountCostForSelfdestruct, 0);
            gp.set(SstoreSetWithoutLoadCost, 2800);
            gp.set(SstoreSetStateGas, 32 * CPSB);
            gp.set(NewAccountStateGas, 112 * CPSB);
            gp.set(CodeDepositStateGas, CPSB);
            gp.set(CreateStateGas, 112 * CPSB);
            gp.set(SstoreSetRefund, 32 * CPSB + 2800);
            gp.set(TxEip7702PerEmptyAccountCost, 7500 + (112 + 23) * CPSB);
            gp.set(TxEip7702AuthRefund, 112 * CPSB);
            gp.set(TxEip7702PerAuthStateGas, (112 + 23) * CPSB);

            new_op!(C, CREATE, ZERO, instr::create<T, false>);
            new_op!(C, CREATE2, ZERO, instr::create<T, true>);
            new_op!(C, SSTORE, ZERO, instr::sstore<T>);
            new_op!(C, CALL, WARM_STORAGE_READ_COST, instr::call<T>);
            new_op!(C, CALLCODE, WARM_STORAGE_READ_COST, instr::callcode<T>);
            new_op!(C, DELEGATECALL, WARM_STORAGE_READ_COST, instr::delegatecall<T>);
            new_op!(C, STATICCALL, WARM_STORAGE_READ_COST, instr::staticcall<T>);
            new_op!(C, SELFDESTRUCT, 5000, instr::selfdestruct<T>);
        }

        Self { spec_id, static_gas_table: gt, gas_params: gp, instruction_impls: i }
    }

    /// Returns the hard fork specification for this version.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.spec_id
    }
}
