//! EVM opcodes, specification IDs and bytecode analysis, with no dependency on an EVM
//! implementation.
//!
//! This crate is `no_std` and builds for `wasm32-unknown-unknown`. It holds the opcode table
//! ([`opcode`]), [`SpecId`], the jump destination analysis ([`analyze_legacy`]) and the
//! disassembly layer ([`Instructions`], [`BasicBlocks`]).

#![no_std]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

extern crate alloc;

#[cfg(any(feature = "std", test))]
extern crate std;

mod analysis;
mod blocks;
mod instructions;
mod jump_table;
pub mod opcode;
mod spec_id;

pub use analysis::analyze_legacy;
pub use blocks::{BasicBlock, BasicBlocks, JumpTarget, Terminator};
pub use instructions::{Instruction, Instructions, decode_pair, decode_single};
pub use jump_table::{JumpTable, JumpTableRef};
pub use spec_id::{ParseSpecIdError, SpecId};
