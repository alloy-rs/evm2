//! EVM version data.

use crate::{
    EvmConfig, EvmTypes,
    interpreter::{SpecId, opcode::op},
};

mod gas_params;
pub use gas_params::{GasId, GasParams, num_words};

mod static_gas_table;
pub use static_gas_table::StaticGasTable;

mod instruction_impl_table;
pub use instruction_impl_table::InstructionImplTable;

/// EVM version data.
#[derive(Debug)]
pub struct Version {
    /// Active hard fork specification.
    pub spec_id: SpecId,
    /// Static opcode gas table.
    pub static_gas_table: StaticGasTable,
    /// Dynamic gas parameter table.
    pub gas_params: GasParams,
}

/// Type-specific EVM version data.
#[derive(Debug)]
pub struct EvmVersion<T: EvmTypes = crate::BaseEvmTypes> {
    /// Active EVM version.
    pub version: &'static Version,
    /// Instruction implementations.
    pub instruction_impls: InstructionImplTable<T>,
}

macro_rules! evm_versions {
    ($($spec:ident { $($tokens:tt)* })*) => {
        impl Version {
            /// Creates an empty EVM version for `spec`.
            #[inline]
            const fn empty(spec_id: SpecId) -> Self {
                Self {
                    spec_id,
                    static_gas_table: StaticGasTable::empty(),
                    gas_params: GasParams::empty(),
                }
            }

            /// Creates the base EVM version for `spec`.
            pub const fn new_base(spec_id: SpecId) -> Self {
                use crate::interpreter::gas::*;
                use GasId::*;

                let mut v = Self::empty(spec_id);

                $(
                    if spec_id.enables(SpecId::$spec) {
                        const C: u8 = SpecId::$spec as u8;
                        let _ = C;

                        #[allow(unused_macros)]
                        macro_rules! op {
                            ($name:ident, $cost:expr, $instr:path) => {
                                v.static_gas_table.set(op::$name, $cost as u16);
                            };
                        }
                        #[allow(unused_macros)]
                        macro_rules! static_gas {
                            ($name:ident, $cost:expr, $instr:path) => {
                                op!($name, $cost, $instr);
                            };
                        }
                        #[allow(unused_macros)]
                        macro_rules! gas {
                            ($id:ident, $value:expr) => {
                                v.gas_params.set($id, $value);
                            };
                        }

                        $($tokens)*
                    }
                )*

                v
            }

            /// Returns the hard fork specification for this version.
            #[inline]
            pub const fn spec_id(&self) -> SpecId {
                self.spec_id
            }
        }

        impl<T: EvmTypes> EvmVersion<T> {
            /// Creates an empty type-specific EVM version.
            #[inline]
            const fn empty(version: &'static Version) -> Self {
                Self { version, instruction_impls: InstructionImplTable::empty() }
            }

            /// Creates the type-specific EVM version for `Cfg`.
            pub const fn new_base<Cfg: EvmConfig>() -> Self {
                use crate::interpreter::{gas::*, instructions as instr};

                let version = Cfg::VERSION;
                let spec_id = version.spec_id;
                let mut v = Self::empty(version);

                $(
                    if spec_id.enables(SpecId::$spec) {
                        const C: u8 = SpecId::$spec as u8;
                        let _ = C;

                        #[allow(unused_macros)]
                        macro_rules! op {
                            ($name:ident, $cost:expr, $instr:path) => {
                                v.instruction_impls.set(
                                    op::$name,
                                    Some(
                                        <$instr as instr::table::Instruction<T>>::execute::<
                                            VersionConfig<C>,
                                        >,
                                    ),
                                );
                            };
                        }
                        #[allow(unused_macros)]
                        macro_rules! static_gas {
                            ($name:ident, $cost:expr, $instr:path) => {
                                op!($name, $cost, $instr);
                            };
                        }
                        #[allow(unused_macros)]
                        macro_rules! gas {
                            ($id:ident, $value:expr) => {
                                let _ = $value;
                            };
                        }

                        $($tokens)*
                    }
                )*

                v
            }
        }
    };
}

evm_versions! {
    FRONTIER {
        op!(STOP, ZERO, instr::stop<T>);
        op!(ADD, VERYLOW, instr::add<T>);
        op!(MUL, LOW, instr::mul<T>);
        op!(SUB, VERYLOW, instr::sub<T>);
        op!(DIV, LOW, instr::div<T>);
        op!(SDIV, LOW, instr::sdiv<T>);
        op!(MOD, LOW, instr::rem<T>);
        op!(SMOD, LOW, instr::smod<T>);
        op!(ADDMOD, MID, instr::addmod<T>);
        op!(MULMOD, MID, instr::mulmod<T>);
        op!(EXP, EXP, instr::exp<T>);
        op!(SIGNEXTEND, LOW, instr::signextend<T>);
        op!(LT, VERYLOW, instr::lt<T>);
        op!(GT, VERYLOW, instr::gt<T>);
        op!(SLT, VERYLOW, instr::slt<T>);
        op!(SGT, VERYLOW, instr::sgt<T>);
        op!(EQ, VERYLOW, instr::eq<T>);
        op!(ISZERO, VERYLOW, instr::iszero<T>);
        op!(AND, VERYLOW, instr::bitand<T>);
        op!(OR, VERYLOW, instr::bitor<T>);
        op!(XOR, VERYLOW, instr::bitxor<T>);
        op!(NOT, VERYLOW, instr::not<T>);
        op!(BYTE, VERYLOW, instr::byte<T>);
        op!(KECCAK256, KECCAK256, instr::keccak256<T>);
        op!(ADDRESS, BASE, instr::address<T>);
        op!(BALANCE, 20, instr::balance<T>);
        op!(ORIGIN, BASE, instr::origin<T>);
        op!(CALLER, BASE, instr::caller<T>);
        op!(CALLVALUE, BASE, instr::callvalue<T>);
        op!(CALLDATALOAD, VERYLOW, instr::calldataload<T>);
        op!(CALLDATASIZE, BASE, instr::calldatasize<T>);
        op!(CALLDATACOPY, VERYLOW, instr::calldatacopy<T>);
        op!(CODESIZE, BASE, instr::codesize<T>);
        op!(CODECOPY, VERYLOW, instr::codecopy<T>);
        op!(GASPRICE, BASE, instr::gasprice<T>);
        op!(EXTCODESIZE, 20, instr::extcodesize<T>);
        op!(EXTCODECOPY, 20, instr::extcodecopy<T>);
        op!(BLOCKHASH, BLOCKHASH, instr::blockhash<T>);
        op!(COINBASE, BASE, instr::coinbase<T>);
        op!(TIMESTAMP, BASE, instr::timestamp<T>);
        op!(NUMBER, BASE, instr::block_number<T>);
        op!(DIFFICULTY, BASE, instr::difficulty<T>);
        op!(GASLIMIT, BASE, instr::gaslimit<T>);
        op!(POP, BASE, instr::pop<T>);
        op!(MLOAD, VERYLOW, instr::mload<T>);
        op!(MSTORE, VERYLOW, instr::mstore<T>);
        op!(MSTORE8, VERYLOW, instr::mstore8<T>);
        op!(SLOAD, 50, instr::sload<T>);
        op!(SSTORE, ZERO, instr::sstore<T>);
        op!(JUMP, MID, instr::jump<T>);
        op!(JUMPI, HIGH, instr::jumpi<T>);
        op!(PC, BASE, instr::pc<T>);
        op!(MSIZE, BASE, instr::msize<T>);
        op!(GAS, BASE, instr::gas<T>);
        op!(JUMPDEST, JUMPDEST, instr::jumpdest<T>);
        op!(PUSH1, VERYLOW, instr::push<T, 1>);
        op!(PUSH2, VERYLOW, instr::push<T, 2>);
        op!(PUSH3, VERYLOW, instr::push<T, 3>);
        op!(PUSH4, VERYLOW, instr::push<T, 4>);
        op!(PUSH5, VERYLOW, instr::push<T, 5>);
        op!(PUSH6, VERYLOW, instr::push<T, 6>);
        op!(PUSH7, VERYLOW, instr::push<T, 7>);
        op!(PUSH8, VERYLOW, instr::push<T, 8>);
        op!(PUSH9, VERYLOW, instr::push<T, 9>);
        op!(PUSH10, VERYLOW, instr::push<T, 10>);
        op!(PUSH11, VERYLOW, instr::push<T, 11>);
        op!(PUSH12, VERYLOW, instr::push<T, 12>);
        op!(PUSH13, VERYLOW, instr::push<T, 13>);
        op!(PUSH14, VERYLOW, instr::push<T, 14>);
        op!(PUSH15, VERYLOW, instr::push<T, 15>);
        op!(PUSH16, VERYLOW, instr::push<T, 16>);
        op!(PUSH17, VERYLOW, instr::push<T, 17>);
        op!(PUSH18, VERYLOW, instr::push<T, 18>);
        op!(PUSH19, VERYLOW, instr::push<T, 19>);
        op!(PUSH20, VERYLOW, instr::push<T, 20>);
        op!(PUSH21, VERYLOW, instr::push<T, 21>);
        op!(PUSH22, VERYLOW, instr::push<T, 22>);
        op!(PUSH23, VERYLOW, instr::push<T, 23>);
        op!(PUSH24, VERYLOW, instr::push<T, 24>);
        op!(PUSH25, VERYLOW, instr::push<T, 25>);
        op!(PUSH26, VERYLOW, instr::push<T, 26>);
        op!(PUSH27, VERYLOW, instr::push<T, 27>);
        op!(PUSH28, VERYLOW, instr::push<T, 28>);
        op!(PUSH29, VERYLOW, instr::push<T, 29>);
        op!(PUSH30, VERYLOW, instr::push<T, 30>);
        op!(PUSH31, VERYLOW, instr::push<T, 31>);
        op!(PUSH32, VERYLOW, instr::push<T, 32>);
        op!(DUP1, VERYLOW, instr::dup<T, 1>);
        op!(DUP2, VERYLOW, instr::dup<T, 2>);
        op!(DUP3, VERYLOW, instr::dup<T, 3>);
        op!(DUP4, VERYLOW, instr::dup<T, 4>);
        op!(DUP5, VERYLOW, instr::dup<T, 5>);
        op!(DUP6, VERYLOW, instr::dup<T, 6>);
        op!(DUP7, VERYLOW, instr::dup<T, 7>);
        op!(DUP8, VERYLOW, instr::dup<T, 8>);
        op!(DUP9, VERYLOW, instr::dup<T, 9>);
        op!(DUP10, VERYLOW, instr::dup<T, 10>);
        op!(DUP11, VERYLOW, instr::dup<T, 11>);
        op!(DUP12, VERYLOW, instr::dup<T, 12>);
        op!(DUP13, VERYLOW, instr::dup<T, 13>);
        op!(DUP14, VERYLOW, instr::dup<T, 14>);
        op!(DUP15, VERYLOW, instr::dup<T, 15>);
        op!(DUP16, VERYLOW, instr::dup<T, 16>);
        op!(SWAP1, VERYLOW, instr::swap<T, 1>);
        op!(SWAP2, VERYLOW, instr::swap<T, 2>);
        op!(SWAP3, VERYLOW, instr::swap<T, 3>);
        op!(SWAP4, VERYLOW, instr::swap<T, 4>);
        op!(SWAP5, VERYLOW, instr::swap<T, 5>);
        op!(SWAP6, VERYLOW, instr::swap<T, 6>);
        op!(SWAP7, VERYLOW, instr::swap<T, 7>);
        op!(SWAP8, VERYLOW, instr::swap<T, 8>);
        op!(SWAP9, VERYLOW, instr::swap<T, 9>);
        op!(SWAP10, VERYLOW, instr::swap<T, 10>);
        op!(SWAP11, VERYLOW, instr::swap<T, 11>);
        op!(SWAP12, VERYLOW, instr::swap<T, 12>);
        op!(SWAP13, VERYLOW, instr::swap<T, 13>);
        op!(SWAP14, VERYLOW, instr::swap<T, 14>);
        op!(SWAP15, VERYLOW, instr::swap<T, 15>);
        op!(SWAP16, VERYLOW, instr::swap<T, 16>);
        op!(LOG0, LOG, instr::log<T, 0>);
        op!(LOG1, LOG, instr::log<T, 1>);
        op!(LOG2, LOG, instr::log<T, 2>);
        op!(LOG3, LOG, instr::log<T, 3>);
        op!(LOG4, LOG, instr::log<T, 4>);
        op!(CREATE, ZERO, instr::create<T, false>);
        op!(CALL, 40, instr::call<T>);
        op!(CALLCODE, 40, instr::callcode<T>);
        op!(RETURN, ZERO, instr::r#return<T>);
        op!(INVALID, ZERO, instr::invalid<T>);
        op!(SELFDESTRUCT, ZERO, instr::selfdestruct<T>);

        gas!(ExpByteGas, 10);
        gas!(Logdata, LOGDATA);
        gas!(Logtopic, LOGTOPIC);
        gas!(CopyPerWord, COPY);
        gas!(ExtcodecopyPerWord, COPY);
        gas!(McopyPerWord, COPY);
        gas!(Keccak256PerWord, KECCAK256WORD);
        gas!(MemoryLinearCost, MEMORY);
        gas!(MemoryQuadraticReduction, 512);
        gas!(InitcodePerWord, INITCODE_WORD_COST);
        gas!(Create, CREATE);
        gas!(CallStipendReduction, 64);
        gas!(TransferValueCost, CALLVALUE);
        gas!(NewAccountCost, NEWACCOUNT);
        gas!(SstoreStatic, SSTORE_RESET);
        gas!(SstoreSetWithoutLoadCost, SSTORE_SET - SSTORE_RESET);
        gas!(SstoreSetRefund, SSTORE_SET - SSTORE_RESET);
        gas!(SstoreClearingSlotRefund, REFUND_SSTORE_CLEARS);
        gas!(SelfdestructRefund, SELFDESTRUCT_REFUND);
        gas!(CallStipend, CALL_STIPEND);
        gas!(CodeDepositCost, CODEDEPOSIT);
        gas!(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER);
        gas!(TxTokenCost, STANDARD_TOKEN_COST);
        gas!(TxBaseStipend, 21000);
    }

    HOMESTEAD {
        op!(DELEGATECALL, 40, instr::delegatecall<T>);
        gas!(TxCreateCost, CREATE);
    }

    TANGERINE {
        gas!(NewAccountCostForSelfdestruct, NEWACCOUNT);

        static_gas!(SLOAD, 200, instr::sload<T>);
        static_gas!(BALANCE, 400, instr::balance<T>);
        static_gas!(EXTCODESIZE, 700, instr::extcodesize<T>);
        static_gas!(EXTCODECOPY, 700, instr::extcodecopy<T>);
        op!(CREATE, ZERO, instr::create<T, false>);
        static_gas!(CALL, 700, instr::call<T>);
        static_gas!(CALLCODE, 700, instr::callcode<T>);
        static_gas!(DELEGATECALL, 700, instr::delegatecall<T>);
        static_gas!(SELFDESTRUCT, 5000, instr::selfdestruct<T>);
    }

    SPURIOUS_DRAGON {
        gas!(ExpByteGas, 50);
        op!(EXP, EXP, instr::exp<T>);
    }

    BYZANTIUM {
        op!(RETURNDATASIZE, BASE, instr::returndatasize<T>);
        op!(RETURNDATACOPY, VERYLOW, instr::returndatacopy<T>);
        op!(STATICCALL, 700, instr::staticcall<T>);
        op!(REVERT, ZERO, instr::revert<T>);
    }

    CONSTANTINOPLE {
        op!(SHL, VERYLOW, instr::shl<T>);
        op!(SHR, VERYLOW, instr::shr<T>);
        op!(SAR, VERYLOW, instr::sar<T>);
        op!(EXTCODEHASH, 400, instr::extcodehash<T>);
    }

    PETERSBURG {
        op!(CREATE2, ZERO, instr::create<T, true>);
    }

    ISTANBUL {
        op!(CHAINID, BASE, instr::chainid<T>);
        op!(SELFBALANCE, LOW, instr::selfbalance<T>);

        gas!(SstoreStatic, ISTANBUL_SLOAD_GAS);
        gas!(SstoreSetWithoutLoadCost, SSTORE_SET - ISTANBUL_SLOAD_GAS);
        gas!(SstoreResetWithoutColdLoadCost, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
        gas!(SstoreSetRefund, SSTORE_SET - ISTANBUL_SLOAD_GAS);
        gas!(SstoreResetRefund, SSTORE_RESET - ISTANBUL_SLOAD_GAS);
        gas!(TxTokenNonZeroByteMultiplier, NON_ZERO_BYTE_MULTIPLIER_ISTANBUL);

        static_gas!(SLOAD, ISTANBUL_SLOAD_GAS, instr::sload<T>);
        static_gas!(BALANCE, 700, instr::balance<T>);
        static_gas!(EXTCODEHASH, 700, instr::extcodehash<T>);
        op!(SSTORE, ZERO, instr::sstore<T>);
    }

    BERLIN {
        gas!(SstoreStatic, WARM_STORAGE_READ_COST);
        gas!(ColdAccountAdditionalCost, COLD_ACCOUNT_ACCESS_COST_ADDITIONAL);
        gas!(ColdStorageAdditionalCost, COLD_SLOAD_COST - WARM_STORAGE_READ_COST);
        gas!(ColdStorageCost, COLD_SLOAD_COST);
        gas!(WarmStorageReadCost, WARM_STORAGE_READ_COST);
        gas!(SstoreResetWithoutColdLoadCost, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
        gas!(SstoreSetWithoutLoadCost, SSTORE_SET - WARM_STORAGE_READ_COST);
        gas!(SstoreSetRefund, SSTORE_SET - WARM_STORAGE_READ_COST);
        gas!(SstoreResetRefund, WARM_SSTORE_RESET - WARM_STORAGE_READ_COST);
        gas!(TxAccessListAddressCost, ACCESS_LIST_ADDRESS);
        gas!(TxAccessListStorageKeyCost, ACCESS_LIST_STORAGE_KEY);

        static_gas!(SLOAD, WARM_STORAGE_READ_COST, instr::sload<T>);
        static_gas!(BALANCE, WARM_STORAGE_READ_COST, instr::balance<T>);
        static_gas!(EXTCODESIZE, WARM_STORAGE_READ_COST, instr::extcodesize<T>);
        static_gas!(EXTCODEHASH, WARM_STORAGE_READ_COST, instr::extcodehash<T>);
        static_gas!(EXTCODECOPY, WARM_STORAGE_READ_COST, instr::extcodecopy<T>);
        op!(SSTORE, ZERO, instr::sstore<T>);
        static_gas!(CALL, WARM_STORAGE_READ_COST, instr::call<T>);
        static_gas!(CALLCODE, WARM_STORAGE_READ_COST, instr::callcode<T>);
        static_gas!(DELEGATECALL, WARM_STORAGE_READ_COST, instr::delegatecall<T>);
        static_gas!(STATICCALL, WARM_STORAGE_READ_COST, instr::staticcall<T>);
        op!(SELFDESTRUCT, 5000, instr::selfdestruct<T>);
    }

    LONDON {
        op!(BASEFEE, BASE, instr::basefee<T>);

        gas!(SstoreClearingSlotRefund, WARM_SSTORE_RESET + ACCESS_LIST_STORAGE_KEY);
        gas!(SelfdestructRefund, 0);

        op!(SSTORE, ZERO, instr::sstore<T>);
        op!(SELFDESTRUCT, 5000, instr::selfdestruct<T>);
    }

    SHANGHAI {
        op!(PUSH0, BASE, instr::push<T, 0>);

        gas!(TxInitcodeCost, INITCODE_WORD_COST);

        op!(CREATE, ZERO, instr::create<T, false>);
        op!(CREATE2, ZERO, instr::create<T, true>);
    }

    CANCUN {
        op!(BLOBHASH, VERYLOW, instr::blobhash<T>);
        op!(BLOBBASEFEE, BASE, instr::blobbasefee<T>);
        op!(TLOAD, WARM_STORAGE_READ_COST, instr::tload<T>);
        op!(TSTORE, WARM_STORAGE_READ_COST, instr::tstore<T>);
        op!(MCOPY, VERYLOW, instr::mcopy<T>);
    }

    PRAGUE {
        gas!(TxEip7702PerEmptyAccountCost, EIP7702_PER_EMPTY_ACCOUNT_COST);
        gas!(TxEip7702AuthRefund, EIP7702_PER_EMPTY_ACCOUNT_COST - EIP7702_PER_AUTH_BASE_COST);
        gas!(TxFloorCostPerToken, TOTAL_COST_FLOOR_PER_TOKEN);
        gas!(TxFloorCostBaseGas, 21000);
    }

    OSAKA {
        op!(CLZ, LOW, instr::clz<T>);
        op!(DUPN, VERYLOW, instr::dupn<T>);
        op!(SWAPN, VERYLOW, instr::swapn<T>);
        op!(EXCHANGE, VERYLOW, instr::exchange<T>);
    }

    AMSTERDAM {
        const CPSB: u32 = 1174;

        op!(SLOTNUM, BASE, instr::slotnum<T>);

        gas!(Create, 9000);
        gas!(TxCreateCost, 9000);
        gas!(CodeDepositCost, 0);
        gas!(NewAccountCost, 0);
        gas!(NewAccountCostForSelfdestruct, 0);
        gas!(SstoreSetWithoutLoadCost, 2800);
        gas!(SstoreSetStateGas, 32 * CPSB);
        gas!(NewAccountStateGas, 112 * CPSB);
        gas!(CodeDepositStateGas, CPSB);
        gas!(CreateStateGas, 112 * CPSB);
        gas!(SstoreSetRefund, 32 * CPSB + 2800);
        gas!(TxEip7702PerEmptyAccountCost, 7500 + (112 + 23) * CPSB);
        gas!(TxEip7702AuthRefund, 112 * CPSB);
        gas!(TxEip7702PerAuthStateGas, (112 + 23) * CPSB);

        op!(CREATE, ZERO, instr::create<T, false>);
        op!(CREATE2, ZERO, instr::create<T, true>);
        op!(SSTORE, ZERO, instr::sstore<T>);
        op!(CALL, WARM_STORAGE_READ_COST, instr::call<T>);
        op!(CALLCODE, WARM_STORAGE_READ_COST, instr::callcode<T>);
        op!(DELEGATECALL, WARM_STORAGE_READ_COST, instr::delegatecall<T>);
        op!(STATICCALL, WARM_STORAGE_READ_COST, instr::staticcall<T>);
        op!(SELFDESTRUCT, 5000, instr::selfdestruct<T>);
    }
}

struct VersionConfig<const SPEC_ID: u8>;

impl<const SPEC_ID: u8> EvmConfig for VersionConfig<SPEC_ID> {
    const VERSION: &'static Version = &Version::new_base(match SpecId::try_from_u8(SPEC_ID) {
        Some(spec_id) => spec_id,
        None => panic!("invalid EVM specification ID"),
    });
}
