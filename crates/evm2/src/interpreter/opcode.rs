macro_rules! opcodes {
    ($d:tt $($val:literal => $name:ident => $instr:path;)*) => {
        $(
            #[doc = concat!("Opcode byte for `", stringify!($name), "`.")]
            pub const $name: u8 = $val;
        )*

        impl OpCode {
            $(
                #[doc = concat!("Opcode metadata for `", stringify!($name), "`.")]
                pub const $name: Self = Self($val);
            )*
        }

        /// Maps each opcode byte to metadata.
        pub const OPCODE_INFO: [Option<OpInfo>; 256] = {
            let mut map = [None; 256];
            $(
                map[$val] = Some(OpInfo { opcode: $val, name: stringify!($name) });
            )*
            map
        };

        /// Maps each opcode name to metadata.
        #[cfg(feature = "parse")]
        pub(crate) static NAME_TO_OPCODE: phf::Map<&'static str, OpCode> =
            stringify_with_cb! { phf_map_cb; $($name)* };
    };
}

/// Callback for creating a [`phf`] map with `stringify_with_cb`.
#[cfg(feature = "parse")]
macro_rules! phf_map_cb {
    ($(#[doc = $s:literal] $id:ident)*) => {
        phf::phf_map! {
            $($s => OpCode::$id),*
        }
    };
}

/// Stringifies identifiers with `paste` so that they are available as literals.
///
/// This doesn't work with [`stringify!`] because it cannot be expanded inside another macro.
#[cfg(feature = "parse")]
macro_rules! stringify_with_cb {
    ($callback:ident; $($id:ident)*) => { paste::paste! {
        $callback! { $(#[doc = "" $id ""] $id)* }
    }};
}

/// Opcode metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpCode(u8);

impl OpCode {
    /// Creates opcode metadata for a raw opcode byte.
    #[inline]
    pub const fn new(opcode: u8) -> Option<Self> {
        if Self::info_by_op(opcode).is_some() { Some(Self(opcode)) } else { None }
    }

    /// Creates opcode metadata for a raw opcode byte without validating it.
    #[inline]
    pub const fn new_or_unknown(opcode: u8) -> Self {
        Self(opcode)
    }

    /// Creates opcode metadata for a raw opcode byte without validation.
    ///
    /// # Safety
    ///
    /// The caller must ensure the raw byte is acceptable for the consumer of the returned opcode.
    #[inline]
    pub const unsafe fn new_unchecked(opcode: u8) -> Self {
        Self(opcode)
    }

    /// Returns the raw opcode byte.
    #[inline]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns opcode info for a raw opcode byte.
    #[inline]
    pub const fn info_by_op(opcode: u8) -> Option<OpInfo> {
        OPCODE_INFO[opcode as usize]
    }

    /// Returns whether this opcode is defined by evm2.
    #[inline]
    pub const fn is_valid(self) -> bool {
        Self::info_by_op(self.0).is_some()
    }

    /// Returns opcode stack and immediate metadata.
    #[inline]
    pub const fn info(self) -> OpInfo {
        if let Some(info) = Self::info_by_op(self.0) { info } else { OpInfo::unknown() }
    }

    /// Returns the opcode name.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        self.info().name()
    }

    /// Returns the immediate byte count.
    #[inline]
    pub const fn immediate_size(self) -> u8 {
        self.info().immediate_size()
    }

    /// Returns the number of stack outputs.
    #[inline]
    pub const fn outputs(self) -> u8 {
        self.info().outputs()
    }

    /// Returns whether the opcode can modify linear memory.
    #[inline]
    pub const fn modifies_memory(self) -> bool {
        self.info().modifies_memory()
    }
}

impl core::fmt::Display for OpCode {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Opcode stack and immediate metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpInfo {
    opcode: u8,
    name: &'static str,
}

impl OpInfo {
    /// Returns unknown opcode metadata.
    #[inline]
    pub const fn unknown() -> Self {
        Self { opcode: 0xFF, name: "UNKNOWN" }
    }

    /// Returns the opcode name.
    #[inline]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the raw opcode byte.
    #[inline]
    pub const fn opcode(self) -> u8 {
        self.opcode
    }

    /// Returns the immediate byte count.
    #[inline]
    pub const fn immediate_size(self) -> u8 {
        if self.opcode >= PUSH1 && self.opcode <= PUSH32 {
            self.opcode - PUSH1 + 1
        } else if matches!(self.opcode, DUPN | SWAPN | EXCHANGE) {
            1
        } else {
            0
        }
    }

    /// Returns the number of stack outputs.
    #[inline]
    pub const fn outputs(self) -> u8 {
        match self.opcode {
            STOP | SSTORE | JUMP | JUMPI | LOG0..=LOG4 | RETURN | REVERT | SELFDESTRUCT => 0,
            CALL | CALLCODE | DELEGATECALL | STATICCALL | CREATE | CREATE2 => 1,
            DUP1..=DUP16 => self.opcode - DUP1 + 2,
            _ => 1,
        }
    }

    /// Returns whether the opcode can modify linear memory.
    #[inline]
    pub const fn modifies_memory(self) -> bool {
        matches!(
            self.opcode,
            MSTORE
                | MSTORE8
                | CALLDATACOPY
                | CODECOPY
                | EXTCODECOPY
                | RETURNDATACOPY
                | MCOPY
                | CALL
                | CALLCODE
                | DELEGATECALL
                | STATICCALL
                | CREATE
                | CREATE2
        )
    }
}

#[cfg(feature = "parse")]
mod parse {
    use super::{NAME_TO_OPCODE, OpCode};
    use core::fmt;

    /// An error indicating that an opcode is invalid.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct OpCodeError(());

    impl fmt::Display for OpCodeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("invalid opcode")
        }
    }

    impl core::error::Error for OpCodeError {}

    impl core::str::FromStr for OpCode {
        type Err = OpCodeError;

        #[inline]
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Self::parse(s).ok_or(OpCodeError(()))
        }
    }

    impl OpCode {
        /// Parses an opcode from a string.
        #[inline]
        pub fn parse(s: &str) -> Option<Self> {
            NAME_TO_OPCODE.get(s).copied()
        }
    }
}

#[cfg(feature = "parse")]
pub use parse::OpCodeError;

opcodes! {$
    0x00 => STOP       => stop;
    0x01 => ADD        => add;
    0x02 => MUL        => mul;
    0x03 => SUB        => sub;
    0x04 => DIV        => div;
    0x05 => SDIV       => sdiv;
    0x06 => MOD        => rem;
    0x07 => SMOD       => smod;
    0x08 => ADDMOD     => addmod;
    0x09 => MULMOD     => mulmod;
    0x0A => EXP        => exp;
    0x0B => SIGNEXTEND => signextend;
    // 0x0C
    // 0x0D
    // 0x0E
    // 0x0F

    0x10 => LT     => lt;
    0x11 => GT     => gt;
    0x12 => SLT    => slt;
    0x13 => SGT    => sgt;
    0x14 => EQ     => eq;
    0x15 => ISZERO => iszero;
    0x16 => AND    => bitand;
    0x17 => OR     => bitor;
    0x18 => XOR    => bitxor;
    0x19 => NOT    => not;
    0x1A => BYTE   => byte;
    0x1B => SHL    => shl;
    0x1C => SHR    => shr;
    0x1D => SAR    => sar;
    0x1E => CLZ    => clz;
    // 0x1F

    0x20 => KECCAK256 => keccak256;
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

    0x30 => ADDRESS        => address;
    0x31 => BALANCE        => balance;
    0x32 => ORIGIN         => origin;
    0x33 => CALLER         => caller;
    0x34 => CALLVALUE      => callvalue;
    0x35 => CALLDATALOAD   => calldataload;
    0x36 => CALLDATASIZE   => calldatasize;
    0x37 => CALLDATACOPY   => calldatacopy;
    0x38 => CODESIZE       => codesize;
    0x39 => CODECOPY       => codecopy;
    0x3A => GASPRICE       => gasprice;
    0x3B => EXTCODESIZE    => extcodesize;
    0x3C => EXTCODECOPY    => extcodecopy;
    0x3D => RETURNDATASIZE => returndatasize;
    0x3E => RETURNDATACOPY => returndatacopy;
    0x3F => EXTCODEHASH    => extcodehash;

    0x40 => BLOCKHASH      => blockhash;
    0x41 => COINBASE       => coinbase;
    0x42 => TIMESTAMP      => timestamp;
    0x43 => NUMBER         => block_number;
    0x44 => DIFFICULTY     => difficulty;
    0x45 => GASLIMIT       => gaslimit;
    0x46 => CHAINID        => chainid;
    0x47 => SELFBALANCE    => selfbalance;
    0x48 => BASEFEE        => basefee;
    0x49 => BLOBHASH       => blobhash;
    0x4A => BLOBBASEFEE    => blobbasefee;
    0x4B => SLOTNUM        => slotnum;
    // 0x4C
    // 0x4D
    // 0x4E
    // 0x4F

    0x50 => POP      => pop;
    0x51 => MLOAD    => mload;
    0x52 => MSTORE   => mstore;
    0x53 => MSTORE8  => mstore8;
    0x54 => SLOAD    => sload;
    0x55 => SSTORE   => sstore;
    0x56 => JUMP     => jump;
    0x57 => JUMPI    => jumpi;
    0x58 => PC       => pc;
    0x59 => MSIZE    => msize;
    0x5A => GAS      => gas;
    0x5B => JUMPDEST => jumpdest;
    0x5C => TLOAD    => tload;
    0x5D => TSTORE   => tstore;
    0x5E => MCOPY    => mcopy;

    0x5F => PUSH0  => push<0>;
    0x60 => PUSH1  => push<1>;
    0x61 => PUSH2  => push<2>;
    0x62 => PUSH3  => push<3>;
    0x63 => PUSH4  => push<4>;
    0x64 => PUSH5  => push<5>;
    0x65 => PUSH6  => push<6>;
    0x66 => PUSH7  => push<7>;
    0x67 => PUSH8  => push<8>;
    0x68 => PUSH9  => push<9>;
    0x69 => PUSH10 => push<10>;
    0x6A => PUSH11 => push<11>;
    0x6B => PUSH12 => push<12>;
    0x6C => PUSH13 => push<13>;
    0x6D => PUSH14 => push<14>;
    0x6E => PUSH15 => push<15>;
    0x6F => PUSH16 => push<16>;
    0x70 => PUSH17 => push<17>;
    0x71 => PUSH18 => push<18>;
    0x72 => PUSH19 => push<19>;
    0x73 => PUSH20 => push<20>;
    0x74 => PUSH21 => push<21>;
    0x75 => PUSH22 => push<22>;
    0x76 => PUSH23 => push<23>;
    0x77 => PUSH24 => push<24>;
    0x78 => PUSH25 => push<25>;
    0x79 => PUSH26 => push<26>;
    0x7A => PUSH27 => push<27>;
    0x7B => PUSH28 => push<28>;
    0x7C => PUSH29 => push<29>;
    0x7D => PUSH30 => push<30>;
    0x7E => PUSH31 => push<31>;
    0x7F => PUSH32 => push<32>;

    0x80 => DUP1  => dup<1>;
    0x81 => DUP2  => dup<2>;
    0x82 => DUP3  => dup<3>;
    0x83 => DUP4  => dup<4>;
    0x84 => DUP5  => dup<5>;
    0x85 => DUP6  => dup<6>;
    0x86 => DUP7  => dup<7>;
    0x87 => DUP8  => dup<8>;
    0x88 => DUP9  => dup<9>;
    0x89 => DUP10 => dup<10>;
    0x8A => DUP11 => dup<11>;
    0x8B => DUP12 => dup<12>;
    0x8C => DUP13 => dup<13>;
    0x8D => DUP14 => dup<14>;
    0x8E => DUP15 => dup<15>;
    0x8F => DUP16 => dup<16>;

    0x90 => SWAP1  => swap<1>;
    0x91 => SWAP2  => swap<2>;
    0x92 => SWAP3  => swap<3>;
    0x93 => SWAP4  => swap<4>;
    0x94 => SWAP5  => swap<5>;
    0x95 => SWAP6  => swap<6>;
    0x96 => SWAP7  => swap<7>;
    0x97 => SWAP8  => swap<8>;
    0x98 => SWAP9  => swap<9>;
    0x99 => SWAP10 => swap<10>;
    0x9A => SWAP11 => swap<11>;
    0x9B => SWAP12 => swap<12>;
    0x9C => SWAP13 => swap<13>;
    0x9D => SWAP14 => swap<14>;
    0x9E => SWAP15 => swap<15>;
    0x9F => SWAP16 => swap<16>;

    0xA0 => LOG0 => log<0>;
    0xA1 => LOG1 => log<1>;
    0xA2 => LOG2 => log<2>;
    0xA3 => LOG3 => log<3>;
    0xA4 => LOG4 => log<4>;
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

    0xE6 => DUPN     => dupn;
    0xE7 => SWAPN    => swapn;
    0xE8 => EXCHANGE => exchange;
    // 0xE9
    // 0xEA
    // 0xEB
    // 0xEC
    // 0xED
    // 0xEE
    // 0xEF

    0xF0 => CREATE       => create<false>;
    0xF1 => CALL         => call;
    0xF2 => CALLCODE     => callcode;
    0xF3 => RETURN       => r#return;
    0xF4 => DELEGATECALL => delegatecall;
    0xF5 => CREATE2      => create<true>;
    // 0xF6
    // 0xF7
    // 0xF8
    // 0xF9
    0xFA => STATICCALL      => staticcall;
    // 0xFB
    // 0xFC
    0xFD => REVERT       => revert;
    0xFE => INVALID      => invalid;
    0xFF => SELFDESTRUCT => selfdestruct;
}
