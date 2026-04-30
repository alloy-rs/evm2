#[cfg(test)]
use super::instructions::{add_impl, balance_impl, invalid_impl, push_impl, stop_impl};

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
    0x00 => STOP       => stop_impl;
    0x01 => ADD        => add_impl;
    0x02 => MUL        => invalid_impl;
    0x03 => SUB        => invalid_impl;
    0x04 => DIV        => invalid_impl;
    0x05 => SDIV       => invalid_impl;
    0x06 => MOD        => invalid_impl;
    0x07 => SMOD       => invalid_impl;
    0x08 => ADDMOD     => invalid_impl;
    0x09 => MULMOD     => invalid_impl;
    0x0A => EXP        => invalid_impl;
    0x0B => SIGNEXTEND => invalid_impl;
    // 0x0C
    // 0x0D
    // 0x0E
    // 0x0F

    0x10 => LT     => invalid_impl;
    0x11 => GT     => invalid_impl;
    0x12 => SLT    => invalid_impl;
    0x13 => SGT    => invalid_impl;
    0x14 => EQ     => invalid_impl;
    0x15 => ISZERO => invalid_impl;
    0x16 => AND    => invalid_impl;
    0x17 => OR     => invalid_impl;
    0x18 => XOR    => invalid_impl;
    0x19 => NOT    => invalid_impl;
    0x1A => BYTE   => invalid_impl;
    0x1B => SHL    => invalid_impl;
    0x1C => SHR    => invalid_impl;
    0x1D => SAR    => invalid_impl;
    0x1E => CLZ    => invalid_impl;
    // 0x1F

    0x20 => KECCAK256 => invalid_impl;
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

    0x30 => ADDRESS      => invalid_impl;
    0x31 => BALANCE      => balance_impl;
    0x32 => ORIGIN       => invalid_impl;
    0x33 => CALLER       => invalid_impl;
    0x34 => CALLVALUE    => invalid_impl;
    0x35 => CALLDATALOAD => invalid_impl;
    0x36 => CALLDATASIZE => invalid_impl;
    0x37 => CALLDATACOPY => invalid_impl;
    0x38 => CODESIZE     => invalid_impl;
    0x39 => CODECOPY     => invalid_impl;

    0x3A => GASPRICE       => invalid_impl;
    0x3B => EXTCODESIZE    => invalid_impl;
    0x3C => EXTCODECOPY    => invalid_impl;
    0x3D => RETURNDATASIZE => invalid_impl;
    0x3E => RETURNDATACOPY => invalid_impl;
    0x3F => EXTCODEHASH    => invalid_impl;
    0x40 => BLOCKHASH      => invalid_impl;
    0x41 => COINBASE       => invalid_impl;
    0x42 => TIMESTAMP      => invalid_impl;
    0x43 => NUMBER         => invalid_impl;
    0x44 => DIFFICULTY     => invalid_impl;
    0x45 => GASLIMIT       => invalid_impl;
    0x46 => CHAINID        => invalid_impl;
    0x47 => SELFBALANCE    => invalid_impl;
    0x48 => BASEFEE        => invalid_impl;
    0x49 => BLOBHASH       => invalid_impl;
    0x4A => BLOBBASEFEE    => invalid_impl;
    0x4B => SLOTNUM        => invalid_impl;
    // 0x4C
    // 0x4D
    // 0x4E
    // 0x4F

    0x50 => POP      => invalid_impl;
    0x51 => MLOAD    => invalid_impl;
    0x52 => MSTORE   => invalid_impl;
    0x53 => MSTORE8  => invalid_impl;
    0x54 => SLOAD    => invalid_impl;
    0x55 => SSTORE   => invalid_impl;
    0x56 => JUMP     => invalid_impl;
    0x57 => JUMPI    => invalid_impl;
    0x58 => PC       => invalid_impl;
    0x59 => MSIZE    => invalid_impl;
    0x5A => GAS      => invalid_impl;
    0x5B => JUMPDEST => invalid_impl;
    0x5C => TLOAD    => invalid_impl;
    0x5D => TSTORE   => invalid_impl;
    0x5E => MCOPY    => invalid_impl;

    0x5F => PUSH0  => push_impl::<0>;
    0x60 => PUSH1  => push_impl::<1>;
    0x61 => PUSH2  => push_impl::<2>;
    0x62 => PUSH3  => push_impl::<3>;
    0x63 => PUSH4  => push_impl::<4>;
    0x64 => PUSH5  => push_impl::<5>;
    0x65 => PUSH6  => push_impl::<6>;
    0x66 => PUSH7  => push_impl::<7>;
    0x67 => PUSH8  => push_impl::<8>;
    0x68 => PUSH9  => push_impl::<9>;
    0x69 => PUSH10 => push_impl::<10>;
    0x6A => PUSH11 => push_impl::<11>;
    0x6B => PUSH12 => push_impl::<12>;
    0x6C => PUSH13 => push_impl::<13>;
    0x6D => PUSH14 => push_impl::<14>;
    0x6E => PUSH15 => push_impl::<15>;
    0x6F => PUSH16 => push_impl::<16>;
    0x70 => PUSH17 => push_impl::<17>;
    0x71 => PUSH18 => push_impl::<18>;
    0x72 => PUSH19 => push_impl::<19>;
    0x73 => PUSH20 => push_impl::<20>;
    0x74 => PUSH21 => push_impl::<21>;
    0x75 => PUSH22 => push_impl::<22>;
    0x76 => PUSH23 => push_impl::<23>;
    0x77 => PUSH24 => push_impl::<24>;
    0x78 => PUSH25 => push_impl::<25>;
    0x79 => PUSH26 => push_impl::<26>;
    0x7A => PUSH27 => push_impl::<27>;
    0x7B => PUSH28 => push_impl::<28>;
    0x7C => PUSH29 => push_impl::<29>;
    0x7D => PUSH30 => push_impl::<30>;
    0x7E => PUSH31 => push_impl::<31>;
    0x7F => PUSH32 => push_impl::<32>;

    0x80 => DUP1  => invalid_impl;
    0x81 => DUP2  => invalid_impl;
    0x82 => DUP3  => invalid_impl;
    0x83 => DUP4  => invalid_impl;
    0x84 => DUP5  => invalid_impl;
    0x85 => DUP6  => invalid_impl;
    0x86 => DUP7  => invalid_impl;
    0x87 => DUP8  => invalid_impl;
    0x88 => DUP9  => invalid_impl;
    0x89 => DUP10 => invalid_impl;
    0x8A => DUP11 => invalid_impl;
    0x8B => DUP12 => invalid_impl;
    0x8C => DUP13 => invalid_impl;
    0x8D => DUP14 => invalid_impl;
    0x8E => DUP15 => invalid_impl;
    0x8F => DUP16 => invalid_impl;

    0x90 => SWAP1  => invalid_impl;
    0x91 => SWAP2  => invalid_impl;
    0x92 => SWAP3  => invalid_impl;
    0x93 => SWAP4  => invalid_impl;
    0x94 => SWAP5  => invalid_impl;
    0x95 => SWAP6  => invalid_impl;
    0x96 => SWAP7  => invalid_impl;
    0x97 => SWAP8  => invalid_impl;
    0x98 => SWAP9  => invalid_impl;
    0x99 => SWAP10 => invalid_impl;
    0x9A => SWAP11 => invalid_impl;
    0x9B => SWAP12 => invalid_impl;
    0x9C => SWAP13 => invalid_impl;
    0x9D => SWAP14 => invalid_impl;
    0x9E => SWAP15 => invalid_impl;
    0x9F => SWAP16 => invalid_impl;

    0xA0 => LOG0 => invalid_impl;
    0xA1 => LOG1 => invalid_impl;
    0xA2 => LOG2 => invalid_impl;
    0xA3 => LOG3 => invalid_impl;
    0xA4 => LOG4 => invalid_impl;
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

    0xE6 => DUPN     => invalid_impl;
    0xE7 => SWAPN    => invalid_impl;
    0xE8 => EXCHANGE => invalid_impl;
    // 0xE9
    // 0xEA
    // 0xEB
    // 0xEC
    // 0xED
    // 0xEE
    // 0xEF

    0xF0 => CREATE       => invalid_impl;
    0xF1 => CALL         => invalid_impl;
    0xF2 => CALLCODE     => invalid_impl;
    0xF3 => RETURN       => invalid_impl;
    0xF4 => DELEGATECALL => invalid_impl;
    0xF5 => CREATE2      => invalid_impl;
    // 0xF6
    // 0xF7
    // 0xF8
    // 0xF9
    0xFA => STATICCALL      => invalid_impl;
    // 0xFB
    // 0xFC
    0xFD => REVERT       => invalid_impl;
    0xFE => INVALID      => invalid_impl;
    0xFF => SELFDESTRUCT => invalid_impl;
}

pub(crate) use for_each_opcode;
