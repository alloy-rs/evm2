#[cfg(test)]
use super::instructions::{add, balance, invalid, push, stop};

macro_rules! opcodes {
    ($d:tt $($val:literal => $name:ident => $f:expr;)*) => {
        pub mod op {
            $(
                pub const $name: u8 = $val;
            )*
        }

        #[cfg(test)]
        const _: () = {
            $(
                let _ = $f;
            )*
        };

        /// Higher-order macro to iterate over all opcodes.
        macro_rules! for_each_opcode {
            ([$d ($d extra:tt)*] $d m:path) => {{
                $m!{[$d($d extra)*]
                    $(
                        ($name, $f),
                    )*
                }
            }};
        }
    };
}

opcodes! {$
    0x00 => STOP       => stop;
    0x01 => ADD        => add;
    0x02 => MUL        => invalid;
    0x03 => SUB        => invalid;
    0x04 => DIV        => invalid;
    0x05 => SDIV       => invalid;
    0x06 => MOD        => invalid;
    0x07 => SMOD       => invalid;
    0x08 => ADDMOD     => invalid;
    0x09 => MULMOD     => invalid;
    0x0A => EXP        => invalid;
    0x0B => SIGNEXTEND => invalid;
    // 0x0C
    // 0x0D
    // 0x0E
    // 0x0F

    0x10 => LT     => invalid;
    0x11 => GT     => invalid;
    0x12 => SLT    => invalid;
    0x13 => SGT    => invalid;
    0x14 => EQ     => invalid;
    0x15 => ISZERO => invalid;
    0x16 => AND    => invalid;
    0x17 => OR     => invalid;
    0x18 => XOR    => invalid;
    0x19 => NOT    => invalid;
    0x1A => BYTE   => invalid;
    0x1B => SHL    => invalid;
    0x1C => SHR    => invalid;
    0x1D => SAR    => invalid;
    0x1E => CLZ    => invalid;
    // 0x1F

    0x20 => KECCAK256 => invalid;
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

    0x30 => ADDRESS      => invalid;
    0x31 => BALANCE      => balance;
    0x32 => ORIGIN       => invalid;
    0x33 => CALLER       => invalid;
    0x34 => CALLVALUE    => invalid;
    0x35 => CALLDATALOAD => invalid;
    0x36 => CALLDATASIZE => invalid;
    0x37 => CALLDATACOPY => invalid;
    0x38 => CODESIZE     => invalid;
    0x39 => CODECOPY     => invalid;

    0x3A => GASPRICE       => invalid;
    0x3B => EXTCODESIZE    => invalid;
    0x3C => EXTCODECOPY    => invalid;
    0x3D => RETURNDATASIZE => invalid;
    0x3E => RETURNDATACOPY => invalid;
    0x3F => EXTCODEHASH    => invalid;
    0x40 => BLOCKHASH      => invalid;
    0x41 => COINBASE       => invalid;
    0x42 => TIMESTAMP      => invalid;
    0x43 => NUMBER         => invalid;
    0x44 => DIFFICULTY     => invalid;
    0x45 => GASLIMIT       => invalid;
    0x46 => CHAINID        => invalid;
    0x47 => SELFBALANCE    => invalid;
    0x48 => BASEFEE        => invalid;
    0x49 => BLOBHASH       => invalid;
    0x4A => BLOBBASEFEE    => invalid;
    0x4B => SLOTNUM        => invalid;
    // 0x4C
    // 0x4D
    // 0x4E
    // 0x4F

    0x50 => POP      => invalid;
    0x51 => MLOAD    => invalid;
    0x52 => MSTORE   => invalid;
    0x53 => MSTORE8  => invalid;
    0x54 => SLOAD    => invalid;
    0x55 => SSTORE   => invalid;
    0x56 => JUMP     => invalid;
    0x57 => JUMPI    => invalid;
    0x58 => PC       => invalid;
    0x59 => MSIZE    => invalid;
    0x5A => GAS      => invalid;
    0x5B => JUMPDEST => invalid;
    0x5C => TLOAD    => invalid;
    0x5D => TSTORE   => invalid;
    0x5E => MCOPY    => invalid;

    0x5F => PUSH0  => push::<0>;
    0x60 => PUSH1  => push::<1>;
    0x61 => PUSH2  => push::<2>;
    0x62 => PUSH3  => push::<3>;
    0x63 => PUSH4  => push::<4>;
    0x64 => PUSH5  => push::<5>;
    0x65 => PUSH6  => push::<6>;
    0x66 => PUSH7  => push::<7>;
    0x67 => PUSH8  => push::<8>;
    0x68 => PUSH9  => push::<9>;
    0x69 => PUSH10 => push::<10>;
    0x6A => PUSH11 => push::<11>;
    0x6B => PUSH12 => push::<12>;
    0x6C => PUSH13 => push::<13>;
    0x6D => PUSH14 => push::<14>;
    0x6E => PUSH15 => push::<15>;
    0x6F => PUSH16 => push::<16>;
    0x70 => PUSH17 => push::<17>;
    0x71 => PUSH18 => push::<18>;
    0x72 => PUSH19 => push::<19>;
    0x73 => PUSH20 => push::<20>;
    0x74 => PUSH21 => push::<21>;
    0x75 => PUSH22 => push::<22>;
    0x76 => PUSH23 => push::<23>;
    0x77 => PUSH24 => push::<24>;
    0x78 => PUSH25 => push::<25>;
    0x79 => PUSH26 => push::<26>;
    0x7A => PUSH27 => push::<27>;
    0x7B => PUSH28 => push::<28>;
    0x7C => PUSH29 => push::<29>;
    0x7D => PUSH30 => push::<30>;
    0x7E => PUSH31 => push::<31>;
    0x7F => PUSH32 => push::<32>;

    0x80 => DUP1  => invalid;
    0x81 => DUP2  => invalid;
    0x82 => DUP3  => invalid;
    0x83 => DUP4  => invalid;
    0x84 => DUP5  => invalid;
    0x85 => DUP6  => invalid;
    0x86 => DUP7  => invalid;
    0x87 => DUP8  => invalid;
    0x88 => DUP9  => invalid;
    0x89 => DUP10 => invalid;
    0x8A => DUP11 => invalid;
    0x8B => DUP12 => invalid;
    0x8C => DUP13 => invalid;
    0x8D => DUP14 => invalid;
    0x8E => DUP15 => invalid;
    0x8F => DUP16 => invalid;

    0x90 => SWAP1  => invalid;
    0x91 => SWAP2  => invalid;
    0x92 => SWAP3  => invalid;
    0x93 => SWAP4  => invalid;
    0x94 => SWAP5  => invalid;
    0x95 => SWAP6  => invalid;
    0x96 => SWAP7  => invalid;
    0x97 => SWAP8  => invalid;
    0x98 => SWAP9  => invalid;
    0x99 => SWAP10 => invalid;
    0x9A => SWAP11 => invalid;
    0x9B => SWAP12 => invalid;
    0x9C => SWAP13 => invalid;
    0x9D => SWAP14 => invalid;
    0x9E => SWAP15 => invalid;
    0x9F => SWAP16 => invalid;

    0xA0 => LOG0 => invalid;
    0xA1 => LOG1 => invalid;
    0xA2 => LOG2 => invalid;
    0xA3 => LOG3 => invalid;
    0xA4 => LOG4 => invalid;
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

    0xE6 => DUPN     => invalid;
    0xE7 => SWAPN    => invalid;
    0xE8 => EXCHANGE => invalid;
    // 0xE9
    // 0xEA
    // 0xEB
    // 0xEC
    // 0xED
    // 0xEE
    // 0xEF

    0xF0 => CREATE       => invalid;
    0xF1 => CALL         => invalid;
    0xF2 => CALLCODE     => invalid;
    0xF3 => RETURN       => invalid;
    0xF4 => DELEGATECALL => invalid;
    0xF5 => CREATE2      => invalid;
    // 0xF6
    // 0xF7
    // 0xF8
    // 0xF9
    0xFA => STATICCALL      => invalid;
    // 0xFB
    // 0xFC
    0xFD => REVERT       => invalid;
    0xFE => INVALID      => invalid;
    0xFF => SELFDESTRUCT => invalid;
}

pub(crate) use for_each_opcode;
