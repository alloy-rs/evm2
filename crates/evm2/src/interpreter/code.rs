//! Frame-local execution cursor over shared code chunks.
//!
//! Account chunks are loaded and warmed by the host. Initcode executes as complete bytecode.
//! This module owns the active allocation, its borrowed view, and every chunk switch.

use super::{BytecodeRef, CodeSource, Gas, Host, InterpreterState, Pc, Result, Word};
use crate::{
    EvmTypesHost,
    bytecode::{CODE_CHUNK_SIZE, COLD_CODE_CHUNK_GAS, CodeChunk, WARM_CODE_CHUNK_GAS},
    evm::CodeChunkLoad,
    trustme,
    utils::word_to_usize_saturated,
};
use alloc::vec::Vec;
use alloy_primitives::Address;

/// Code ownership and execution position for one interpreter frame.
#[derive(Debug)]
pub(super) struct InterpreterCode {
    active: CodeSource,
    // Borrows immutable allocations owned by `active`. Accessors shorten the erased lifetime;
    // activation clears this view before replacing the owning chunk.
    view: Option<BytecodeRef<'static>>,
    pub(super) pc: *const u8,
}

impl Default for InterpreterCode {
    fn default() -> Self {
        Self::new(&CodeSource::default())
    }
}

impl InterpreterCode {
    pub(super) fn new(code: &CodeSource) -> Self {
        let active = code.clone();
        let pc = active.as_bytecode().bytes_slice().as_ptr();
        Self { active, pc, view: None }
    }

    pub(super) const fn active(&self) -> &CodeSource {
        &self.active
    }

    pub(super) const fn is_account(&self) -> bool {
        matches!(self.active, CodeSource::Account(_))
    }

    pub(super) fn view(&self) -> BytecodeRef<'_> {
        self.view.unwrap_or_else(|| BytecodeRef::new(self.active.as_bytecode()))
    }

    /// Only called during instruction execution, after `prepare` initialized the view.
    pub(super) const fn prepared_view(&self) -> BytecodeRef<'_> {
        // SAFETY: `prepare` and every chunk switch initialize the view before execution.
        unsafe { self.view.unwrap_unchecked() }
    }

    pub(super) fn prepare(&mut self) {
        if self.view.is_none() {
            // SAFETY: The chunk owns immutable heap allocations, so moving this structure
            // leaves the view valid. No accessor exposes the erased lifetime.
            let bytecode = unsafe { trustme::decouple_lt(self.active.as_bytecode()) };
            self.view = Some(BytecodeRef::new(bytecode));
        }
    }

    pub(super) fn activate(&mut self, chunk: CodeChunk) {
        self.view = None;
        self.active = CodeSource::Account(chunk);
        self.pc = self.active.as_bytecode().bytes_slice().as_ptr();
        self.prepare();
    }

    pub(super) fn pc(&self) -> usize {
        self.global_pc(Pc::new(self.pc))
    }

    pub(super) fn set_pc(&mut self, pc: usize) {
        let local = pc
            .checked_sub(self.active.index() as usize * CODE_CHUNK_SIZE)
            .expect("PC is outside the active chunk");
        if let CodeSource::Account(CodeChunk::Prepared(chunk)) = &self.active {
            assert!(local < chunk.payload_len(), "PC is outside the logical payload");
        }
        let bytes = self.active.as_bytecode().bytes_slice();
        assert!(local < bytes.len());
        // SAFETY: The local offset is inside the active allocation.
        self.pc = unsafe { bytes.as_ptr().add(local) };
    }

    pub(super) fn global_pc(&self, pc: Pc) -> usize {
        // SAFETY: Every execution PC points into the active chunk.
        let local =
            unsafe { pc.as_ptr().offset_from(self.active.as_bytecode().bytes_slice().as_ptr()) };
        local as usize + self.active.index() as usize * CODE_CHUNK_SIZE
    }

    pub(super) fn is_generated_tail(&self, pc: Pc) -> bool {
        matches!(&self.active, CodeSource::Account(CodeChunk::Prepared(chunk))
            if self.view().pc_offset(pc) == chunk.tail_offset())
    }
}

impl<T: EvmTypesHost> InterpreterState<'_, '_, T> {
    /// Whether this frame loads account code through the host.
    pub const fn is_account_code(&self) -> bool {
        self.0.code.is_account()
    }

    /// Complete logical code size, excluding execution padding.
    pub fn code_size(&self) -> usize {
        self.0.code.active().code_size()
    }

    /// Active payload index in the logical contract.
    pub const fn code_chunk_index(&self) -> u32 {
        self.0.code.active().index()
    }

    /// Original global offset of the active instruction.
    pub fn global_pc(&self, pc: Pc) -> usize {
        self.0.code.global_pc(pc)
    }

    /// Authenticates the generated tail by its recorded offset, never by opcode alone.
    pub(crate) fn is_generated_tail(&self, pc: Pc) -> bool {
        self.0.code.is_generated_tail(pc)
    }

    /// Returns the active account buffer; only valid for account execution.
    pub(crate) fn active_chunk(&self) -> &CodeChunk {
        match self.0.code.active() {
            CodeSource::Account(chunk) => chunk,
            CodeSource::Direct(_) => unreachable!("account code required"),
        }
    }

    /// Loads an account execution buffer and its prior coldness in one host access.
    pub(crate) fn execution_code_chunk(
        &mut self,
        address: &Address,
        index: u32,
        skip_cold: bool,
    ) -> Result<CodeChunkLoad> {
        self.host()
            .load_code_chunk(address, index, skip_cold)
            .map_err(|error| self.fail(error))?
            .ok_or_else(|| {
                self.fail(crate::DatabaseError::new(
                    MissingRuntimeChunk { address: *address, index },
                    true,
                ))
            })
    }

    /// Replaces the owner before rebuilding its borrowed view. Callers reset their PC next.
    pub(crate) fn activate_code_chunk(&mut self, chunk: CodeChunk) {
        self.0.code.activate(chunk);
    }

    /// Pays the warm charge first, skipping a cold load if its premium is unaffordable.
    pub(crate) fn load_and_charge_code_chunk(
        &mut self,
        gas: &mut Gas,
        address: &Address,
        index: u32,
    ) -> Result<CodeChunk> {
        gas.spend(WARM_CODE_CHUNK_GAS)?;
        let cold_additional = COLD_CODE_CHUNK_GAS - WARM_CODE_CHUNK_GAS;
        let loaded =
            self.execution_code_chunk(address, index, gas.remaining() < cold_additional)?;
        if loaded.is_cold {
            gas.spend(cold_additional)?;
        }
        Ok(loaded.chunk)
    }

    /// Loads and charges each chunk once, stopping before any unaffordable cold read.
    pub(crate) fn load_code_range(
        &mut self,
        gas: &mut Gas,
        code_owner: Address,
        source: Word,
        len: usize,
        size: usize,
        multi: bool,
    ) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let source = word_to_usize_saturated(source);
        if source >= size {
            return Ok(Vec::new());
        }
        let end = source.saturating_add(len).min(size);
        let (first, last) = if multi {
            ((source / CODE_CHUNK_SIZE) as u32, ((end - 1) / CODE_CHUNK_SIZE) as u32)
        } else {
            (0, 0)
        };
        let mut bytes = Vec::new();
        for index in first..=last {
            let chunk = self.load_and_charge_code_chunk(gas, &code_owner, index)?;
            let base = if multi { index as usize * CODE_CHUNK_SIZE } else { 0 };
            let from = source.saturating_sub(base).min(chunk.payload_len());
            let to = end.saturating_sub(base).min(chunk.payload_len());
            if from < to {
                bytes.extend_from_slice(&chunk.bytes()[from..to]);
            }
        }
        Ok(bytes)
    }
}

/// A required payload unexpectedly absent from a custom host implementation.
#[derive(Debug, thiserror::Error)]
#[error("missing required runtime chunk {index} for {address}")]
struct MissingRuntimeChunk {
    address: Address,
    index: u32,
}

#[cfg(all(test, feature = "account-ext"))]
mod tests;
