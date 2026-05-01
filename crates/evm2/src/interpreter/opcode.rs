#[cfg(test)]
use super::instructions::*;

macro_rules! opcodes {
    ($d:tt $($min:ident => $val:literal => $name:ident => $instr:path;)*) => {
        /// Opcode byte constants.
        pub mod op {
            $(
                #[doc = concat!("Opcode byte for `", stringify!($name), "`.")]
                pub const $name: u8 = $val;
            )*
        }

        #[cfg(test)]
        const _: () = {
            $(
                let _ = core::mem::size_of::<$instr>();
            )*
        };

        /// Higher-order macro to iterate over all opcodes.
        macro_rules! for_each_opcode {
            ([$d ($d extra:tt)*] $d m:path) => {
                $m!{[$d($d extra)*]
                    $(
                        ($name, $instr, $min),
                    )*
                }
            };
        }
    };
}

opcodes! {$
    FRONTIER => 0x00 => STOP       => stop;
    FRONTIER => 0x01 => ADD        => add;
    FRONTIER => 0x02 => MUL        => mul;
    FRONTIER => 0x03 => SUB        => sub;
    FRONTIER => 0x04 => DIV        => div;
    FRONTIER => 0x05 => SDIV       => sdiv;
    FRONTIER => 0x06 => MOD        => rem;
    FRONTIER => 0x07 => SMOD       => smod;
    FRONTIER => 0x08 => ADDMOD     => addmod;
    FRONTIER => 0x09 => MULMOD     => mulmod;
    FRONTIER => 0x0A => EXP        => exp;
    FRONTIER => 0x0B => SIGNEXTEND => signextend;
    // 0x0C
    // 0x0D
    // 0x0E
    // 0x0F

    FRONTIER => 0x10 => LT     => lt;
    FRONTIER => 0x11 => GT     => gt;
    FRONTIER => 0x12 => SLT    => slt;
    FRONTIER => 0x13 => SGT    => sgt;
    FRONTIER => 0x14 => EQ     => eq;
    FRONTIER => 0x15 => ISZERO => iszero;
    FRONTIER => 0x16 => AND    => bitand;
    FRONTIER => 0x17 => OR     => bitor;
    FRONTIER => 0x18 => XOR    => bitxor;
    FRONTIER => 0x19 => NOT    => not;
    FRONTIER => 0x1A => BYTE   => byte;
    CONSTANTINOPLE => 0x1B => SHL    => shl;
    CONSTANTINOPLE => 0x1C => SHR    => shr;
    CONSTANTINOPLE => 0x1D => SAR    => sar;
    OSAKA => 0x1E => CLZ    => clz;
    // 0x1F

    FRONTIER => 0x20 => KECCAK256 => keccak256;
    // 0x21
    // 0x22
    // 0x23
    // 0x24
    // 0x25
    // 0x26
    // 0x27
    // 0x28
    // 0x29
    // 0x2A
    // 0x2B
    // 0x2C
    // 0x2D
    // 0x2E
    // 0x2F

    FRONTIER => 0x30 => ADDRESS        => address;
    FRONTIER => 0x31 => BALANCE        => balance;
    FRONTIER => 0x32 => ORIGIN         => origin;
    FRONTIER => 0x33 => CALLER         => caller;
    FRONTIER => 0x34 => CALLVALUE      => callvalue;
    FRONTIER => 0x35 => CALLDATALOAD   => calldataload;
    FRONTIER => 0x36 => CALLDATASIZE   => calldatasize;
    FRONTIER => 0x37 => CALLDATACOPY   => calldatacopy;
    FRONTIER => 0x38 => CODESIZE       => codesize;
    FRONTIER => 0x39 => CODECOPY       => codecopy;
    FRONTIER => 0x3A => GASPRICE       => gasprice;
    FRONTIER => 0x3B => EXTCODESIZE    => extcodesize;
    FRONTIER => 0x3C => EXTCODECOPY    => extcodecopy;
    BYZANTIUM => 0x3D => RETURNDATASIZE => returndatasize;
    BYZANTIUM => 0x3E => RETURNDATACOPY => returndatacopy;
    CONSTANTINOPLE => 0x3F => EXTCODEHASH    => extcodehash;

    FRONTIER => 0x40 => BLOCKHASH      => blockhash;
    FRONTIER => 0x41 => COINBASE       => coinbase;
    FRONTIER => 0x42 => TIMESTAMP      => timestamp;
    FRONTIER => 0x43 => NUMBER         => block_number;
    FRONTIER => 0x44 => DIFFICULTY     => difficulty;
    FRONTIER => 0x45 => GASLIMIT       => gaslimit;
    ISTANBUL => 0x46 => CHAINID        => chainid;
    ISTANBUL => 0x47 => SELFBALANCE    => selfbalance;
    LONDON => 0x48 => BASEFEE        => basefee;
    CANCUN => 0x49 => BLOBHASH       => blobhash;
    CANCUN => 0x4A => BLOBBASEFEE    => blobbasefee;
    AMSTERDAM => 0x4B => SLOTNUM        => slotnum;
    // 0x4C
    // 0x4D
    // 0x4E
    // 0x4F

    FRONTIER => 0x50 => POP      => pop;
    FRONTIER => 0x51 => MLOAD    => mload;
    FRONTIER => 0x52 => MSTORE   => mstore;
    FRONTIER => 0x53 => MSTORE8  => mstore8;
    FRONTIER => 0x54 => SLOAD    => sload;
    FRONTIER => 0x55 => SSTORE   => sstore;
    FRONTIER => 0x56 => JUMP     => jump;
    FRONTIER => 0x57 => JUMPI    => jumpi;
    FRONTIER => 0x58 => PC       => pc;
    FRONTIER => 0x59 => MSIZE    => msize;
    FRONTIER => 0x5A => GAS      => gas;
    FRONTIER => 0x5B => JUMPDEST => jumpdest;
    CANCUN => 0x5C => TLOAD    => tload;
    CANCUN => 0x5D => TSTORE   => tstore;
    CANCUN => 0x5E => MCOPY    => mcopy;

    SHANGHAI => 0x5F => PUSH0  => push<0>;
    FRONTIER => 0x60 => PUSH1  => push<1>;
    FRONTIER => 0x61 => PUSH2  => push<2>;
    FRONTIER => 0x62 => PUSH3  => push<3>;
    FRONTIER => 0x63 => PUSH4  => push<4>;
    FRONTIER => 0x64 => PUSH5  => push<5>;
    FRONTIER => 0x65 => PUSH6  => push<6>;
    FRONTIER => 0x66 => PUSH7  => push<7>;
    FRONTIER => 0x67 => PUSH8  => push<8>;
    FRONTIER => 0x68 => PUSH9  => push<9>;
    FRONTIER => 0x69 => PUSH10 => push<10>;
    FRONTIER => 0x6A => PUSH11 => push<11>;
    FRONTIER => 0x6B => PUSH12 => push<12>;
    FRONTIER => 0x6C => PUSH13 => push<13>;
    FRONTIER => 0x6D => PUSH14 => push<14>;
    FRONTIER => 0x6E => PUSH15 => push<15>;
    FRONTIER => 0x6F => PUSH16 => push<16>;
    FRONTIER => 0x70 => PUSH17 => push<17>;
    FRONTIER => 0x71 => PUSH18 => push<18>;
    FRONTIER => 0x72 => PUSH19 => push<19>;
    FRONTIER => 0x73 => PUSH20 => push<20>;
    FRONTIER => 0x74 => PUSH21 => push<21>;
    FRONTIER => 0x75 => PUSH22 => push<22>;
    FRONTIER => 0x76 => PUSH23 => push<23>;
    FRONTIER => 0x77 => PUSH24 => push<24>;
    FRONTIER => 0x78 => PUSH25 => push<25>;
    FRONTIER => 0x79 => PUSH26 => push<26>;
    FRONTIER => 0x7A => PUSH27 => push<27>;
    FRONTIER => 0x7B => PUSH28 => push<28>;
    FRONTIER => 0x7C => PUSH29 => push<29>;
    FRONTIER => 0x7D => PUSH30 => push<30>;
    FRONTIER => 0x7E => PUSH31 => push<31>;
    FRONTIER => 0x7F => PUSH32 => push<32>;

    FRONTIER => 0x80 => DUP1  => dup<1>;
    FRONTIER => 0x81 => DUP2  => dup<2>;
    FRONTIER => 0x82 => DUP3  => dup<3>;
    FRONTIER => 0x83 => DUP4  => dup<4>;
    FRONTIER => 0x84 => DUP5  => dup<5>;
    FRONTIER => 0x85 => DUP6  => dup<6>;
    FRONTIER => 0x86 => DUP7  => dup<7>;
    FRONTIER => 0x87 => DUP8  => dup<8>;
    FRONTIER => 0x88 => DUP9  => dup<9>;
    FRONTIER => 0x89 => DUP10 => dup<10>;
    FRONTIER => 0x8A => DUP11 => dup<11>;
    FRONTIER => 0x8B => DUP12 => dup<12>;
    FRONTIER => 0x8C => DUP13 => dup<13>;
    FRONTIER => 0x8D => DUP14 => dup<14>;
    FRONTIER => 0x8E => DUP15 => dup<15>;
    FRONTIER => 0x8F => DUP16 => dup<16>;

    FRONTIER => 0x90 => SWAP1  => swap<1>;
    FRONTIER => 0x91 => SWAP2  => swap<2>;
    FRONTIER => 0x92 => SWAP3  => swap<3>;
    FRONTIER => 0x93 => SWAP4  => swap<4>;
    FRONTIER => 0x94 => SWAP5  => swap<5>;
    FRONTIER => 0x95 => SWAP6  => swap<6>;
    FRONTIER => 0x96 => SWAP7  => swap<7>;
    FRONTIER => 0x97 => SWAP8  => swap<8>;
    FRONTIER => 0x98 => SWAP9  => swap<9>;
    FRONTIER => 0x99 => SWAP10 => swap<10>;
    FRONTIER => 0x9A => SWAP11 => swap<11>;
    FRONTIER => 0x9B => SWAP12 => swap<12>;
    FRONTIER => 0x9C => SWAP13 => swap<13>;
    FRONTIER => 0x9D => SWAP14 => swap<14>;
    FRONTIER => 0x9E => SWAP15 => swap<15>;
    FRONTIER => 0x9F => SWAP16 => swap<16>;

    FRONTIER => 0xA0 => LOG0 => log<0>;
    FRONTIER => 0xA1 => LOG1 => log<1>;
    FRONTIER => 0xA2 => LOG2 => log<2>;
    FRONTIER => 0xA3 => LOG3 => log<3>;
    FRONTIER => 0xA4 => LOG4 => log<4>;
    // 0xA5
    // 0xA6
    // 0xA7
    // 0xA8
    // 0xA9
    // 0xAA
    // 0xAB
    // 0xAC
    // 0xAD
    // 0xAE
    // 0xAF
    // 0xB0
    // 0xB1
    // 0xB2
    // 0xB3
    // 0xB4
    // 0xB5
    // 0xB6
    // 0xB7
    // 0xB8
    // 0xB9
    // 0xBA
    // 0xBB
    // 0xBC
    // 0xBD
    // 0xBE
    // 0xBF
    // 0xC0
    // 0xC1
    // 0xC2
    // 0xC3
    // 0xC4
    // 0xC5
    // 0xC6
    // 0xC7
    // 0xC8
    // 0xC9
    // 0xCA
    // 0xCB
    // 0xCC
    // 0xCD
    // 0xCE
    // 0xCF
    // 0xD0
    // 0xD1
    // 0xD2
    // 0xD3
    // 0xD4
    // 0xD5
    // 0xD6
    // 0xD7
    // 0xD8
    // 0xD9
    // 0xDA
    // 0xDB
    // 0xDC
    // 0xDD
    // 0xDE
    // 0xDF
    // 0xE0
    // 0xE1
    // 0xE2
    // 0xE3
    // 0xE4
    // 0xE5

    OSAKA => 0xE6 => DUPN     => dupn;
    OSAKA => 0xE7 => SWAPN    => swapn;
    OSAKA => 0xE8 => EXCHANGE => exchange;
    // 0xE9
    // 0xEA
    // 0xEB
    // 0xEC
    // 0xED
    // 0xEE
    // 0xEF

    FRONTIER => 0xF0 => CREATE       => create<false>;
    FRONTIER => 0xF1 => CALL         => call;
    FRONTIER => 0xF2 => CALLCODE     => callcode;
    FRONTIER => 0xF3 => RETURN       => r#return;
    HOMESTEAD => 0xF4 => DELEGATECALL => delegatecall;
    PETERSBURG => 0xF5 => CREATE2      => create<true>;
    // 0xF6
    // 0xF7
    // 0xF8
    // 0xF9
    BYZANTIUM => 0xFA => STATICCALL      => staticcall;
    // 0xFB
    // 0xFC
    BYZANTIUM => 0xFD => REVERT       => revert;
    FRONTIER => 0xFE => INVALID      => invalid;
    FRONTIER => 0xFF => SELFDESTRUCT => selfdestruct;
}

pub(crate) use for_each_opcode;
