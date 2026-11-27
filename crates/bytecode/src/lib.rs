//! EVM opcodes, specification IDs and bytecode analysis, with no dependency on an EVM
//! implementation.
//!
//! This crate is `no_std` and builds for `wasm32-unknown-unknown`. It holds the opcode table
//! ([`opcode`]), [`SpecId`], the jump destination analysis ([`analyze_legacy`]) and the
//! instruction decoding ([`Instructions`], [`InstructionMap`]).

#![no_std]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

extern crate alloc;

#[cfg(any(feature = "std", test))]
extern crate std;

mod analysis;
mod eip8024;
mod instruction_map;
mod instructions;
mod jump_table;
mod memory;
pub mod opcode;
mod spec_id;

pub use analysis::analyze_legacy;
pub use eip8024::{decode_pair, decode_single, encode_pair, encode_single};
pub use instruction_map::InstructionMap;
pub use instructions::{Instruction, Instructions};
pub use jump_table::{JumpTable, JumpTableRef};
pub use spec_id::{ParseSpecIdError, SpecId};
