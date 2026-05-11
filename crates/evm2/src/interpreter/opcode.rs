macro_rules! opcodes {
    ($d:tt $($val:literal => $name:ident => $instr:path;)*) => {
        /// Opcode byte constants.
        pub mod op {
            $(
                #[doc = concat!("Opcode byte for `", stringify!($name), "`.")]
                pub const $name: u8 = $val;
            )*
        }
    };
}

/// Opcode metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpCode(u8);

impl OpCode {
    /// Creates opcode metadata for a raw opcode byte.
    #[inline]
    pub const fn new(opcode: u8) -> Option<Self> {
        Some(Self(opcode))
    }

    /// Creates opcode metadata for a raw opcode byte without validation.
    #[inline]
    pub const unsafe fn new_unchecked(opcode: u8) -> Self {
        Self(opcode)
    }

    /// Returns the raw opcode byte.
    #[inline]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether this opcode is defined by evm2.
    #[inline]
    pub const fn is_valid(self) -> bool {
        matches!(
            self.0,
            op::STOP
                | op::ADD
                | op::MUL
                | op::SUB
                | op::DIV
                | op::SDIV
                | op::MOD
                | op::SMOD
                | op::ADDMOD
                | op::MULMOD
                | op::EXP
                | op::SIGNEXTEND
                | op::LT
                | op::GT
                | op::SLT
                | op::SGT
                | op::EQ
                | op::ISZERO
                | op::AND
                | op::OR
                | op::XOR
                | op::NOT
                | op::BYTE
                | op::SHL
                | op::SHR
                | op::SAR
                | op::CLZ
                | op::KECCAK256
                | op::ADDRESS
                | op::BALANCE
                | op::ORIGIN
                | op::CALLER
                | op::CALLVALUE
                | op::CALLDATALOAD
                | op::CALLDATASIZE
                | op::CALLDATACOPY
                | op::CODESIZE
                | op::CODECOPY
                | op::GASPRICE
                | op::EXTCODESIZE
                | op::EXTCODECOPY
                | op::EXTCODEHASH
                | op::BLOCKHASH
                | op::COINBASE
                | op::TIMESTAMP
                | op::NUMBER
                | op::DIFFICULTY
                | op::GASLIMIT
                | op::CHAINID
                | op::SELFBALANCE
                | op::BASEFEE
                | op::BLOBHASH
                | op::BLOBBASEFEE
                | op::SLOTNUM
                | op::POP
                | op::MLOAD
                | op::MSTORE
                | op::MSTORE8
                | op::SLOAD
                | op::SSTORE
                | op::JUMP
                | op::JUMPI
                | op::PC
                | op::MSIZE
                | op::GAS
                | op::JUMPDEST
                | op::TLOAD
                | op::TSTORE
                | op::MCOPY
                | op::PUSH0..=op::PUSH32
                | op::DUP1..=op::DUP16
                | op::SWAP1..=op::SWAP16
                | op::LOG0..=op::LOG4
                | op::DUPN
                | op::SWAPN
                | op::EXCHANGE
                | op::CREATE
                | op::CALL
                | op::CALLCODE
                | op::RETURN
                | op::DELEGATECALL
                | op::CREATE2
                | op::STATICCALL
                | op::REVERT
                | op::INVALID
                | op::SELFDESTRUCT
        )
    }

    /// Returns opcode stack and immediate metadata.
    #[inline]
    pub const fn info(self) -> OpInfo {
        OpInfo { opcode: self.0 }
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
        f.write_str(opcode_name(self.0))
    }
}

/// Opcode stack and immediate metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpInfo {
    opcode: u8,
}

impl OpInfo {
    /// Returns the immediate byte count.
    #[inline]
    pub const fn immediate_size(self) -> u8 {
        if self.opcode >= op::PUSH1 && self.opcode <= op::PUSH32 {
            self.opcode - op::PUSH1 + 1
        } else if self.opcode == op::DUPN || self.opcode == op::SWAPN {
            1
        } else if self.opcode == op::EXCHANGE {
            1
        } else {
            0
        }
    }

    /// Returns the number of stack outputs.
    #[inline]
    pub const fn outputs(self) -> u8 {
        match self.opcode {
            op::STOP | op::SSTORE | op::JUMP | op::JUMPI | op::LOG0..=op::LOG4 | op::RETURN
            | op::REVERT | op::SELFDESTRUCT => 0,
            op::CALL | op::CALLCODE | op::DELEGATECALL | op::STATICCALL | op::CREATE
            | op::CREATE2 => 1,
            op::DUP1..=op::DUP16 => self.opcode - op::DUP1 + 2,
            _ => 1,
        }
    }

    /// Returns whether the opcode can modify linear memory.
    #[inline]
    pub const fn modifies_memory(self) -> bool {
        matches!(
            self.opcode,
            op::MSTORE
                | op::MSTORE8
                | op::CALLDATACOPY
                | op::CODECOPY
                | op::EXTCODECOPY
                | op::RETURNDATACOPY
                | op::MCOPY
                | op::CALL
                | op::CALLCODE
                | op::DELEGATECALL
                | op::STATICCALL
                | op::CREATE
                | op::CREATE2
        )
    }
}

#[inline]
const fn opcode_name(opcode: u8) -> &'static str {
    match opcode {
        op::STOP => "STOP",
        op::ADD => "ADD",
        op::MUL => "MUL",
        op::SUB => "SUB",
        op::DIV => "DIV",
        op::SDIV => "SDIV",
        op::MOD => "MOD",
        op::SMOD => "SMOD",
        op::ADDMOD => "ADDMOD",
        op::MULMOD => "MULMOD",
        op::EXP => "EXP",
        op::SIGNEXTEND => "SIGNEXTEND",
        op::LT => "LT",
        op::GT => "GT",
        op::SLT => "SLT",
        op::SGT => "SGT",
        op::EQ => "EQ",
        op::ISZERO => "ISZERO",
        op::AND => "AND",
        op::OR => "OR",
        op::XOR => "XOR",
        op::NOT => "NOT",
        op::BYTE => "BYTE",
        op::SHL => "SHL",
        op::SHR => "SHR",
        op::SAR => "SAR",
        op::CLZ => "CLZ",
        op::KECCAK256 => "KECCAK256",
        op::ADDRESS => "ADDRESS",
        op::BALANCE => "BALANCE",
        op::ORIGIN => "ORIGIN",
        op::CALLER => "CALLER",
        op::CALLVALUE => "CALLVALUE",
        op::CALLDATALOAD => "CALLDATALOAD",
        op::CALLDATASIZE => "CALLDATASIZE",
        op::CALLDATACOPY => "CALLDATACOPY",
        op::CODESIZE => "CODESIZE",
        op::CODECOPY => "CODECOPY",
        op::GASPRICE => "GASPRICE",
        op::EXTCODESIZE => "EXTCODESIZE",
        op::EXTCODECOPY => "EXTCODECOPY",
        op::EXTCODEHASH => "EXTCODEHASH",
        op::BLOCKHASH => "BLOCKHASH",
        op::COINBASE => "COINBASE",
        op::TIMESTAMP => "TIMESTAMP",
        op::NUMBER => "NUMBER",
        op::DIFFICULTY => "DIFFICULTY",
        op::GASLIMIT => "GASLIMIT",
        op::CHAINID => "CHAINID",
        op::SELFBALANCE => "SELFBALANCE",
        op::BASEFEE => "BASEFEE",
        op::BLOBHASH => "BLOBHASH",
        op::BLOBBASEFEE => "BLOBBASEFEE",
        op::SLOTNUM => "SLOTNUM",
        op::POP => "POP",
        op::MLOAD => "MLOAD",
        op::MSTORE => "MSTORE",
        op::MSTORE8 => "MSTORE8",
        op::SLOAD => "SLOAD",
        op::SSTORE => "SSTORE",
        op::JUMP => "JUMP",
        op::JUMPI => "JUMPI",
        op::PC => "PC",
        op::MSIZE => "MSIZE",
        op::GAS => "GAS",
        op::JUMPDEST => "JUMPDEST",
        op::TLOAD => "TLOAD",
        op::TSTORE => "TSTORE",
        op::MCOPY => "MCOPY",
        op::PUSH0 => "PUSH0",
        op::PUSH1..=op::PUSH32 => "PUSH",
        op::DUP1..=op::DUP16 => "DUP",
        op::SWAP1..=op::SWAP16 => "SWAP",
        op::LOG0..=op::LOG4 => "LOG",
        op::DUPN => "DUPN",
        op::SWAPN => "SWAPN",
        op::EXCHANGE => "EXCHANGE",
        op::CREATE => "CREATE",
        op::CALL => "CALL",
        op::CALLCODE => "CALLCODE",
        op::RETURN => "RETURN",
        op::DELEGATECALL => "DELEGATECALL",
        op::CREATE2 => "CREATE2",
        op::STATICCALL => "STATICCALL",
        op::REVERT => "REVERT",
        op::INVALID => "INVALID",
        op::SELFDESTRUCT => "SELFDESTRUCT",
        _ => "UNKNOWN",
    }
}

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
