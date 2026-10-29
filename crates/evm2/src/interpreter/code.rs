//! Frame-local execution cursor over shared code chunks.
//!
//! Account chunks are loaded and warmed by the host. Initcode executes as complete bytecode.
//! This module owns the active allocation, its borrowed view, and every chunk switch.

use super::{BytecodeRef, Gas, Host, InstrStop, InterpreterState, Pc, Result, Word};
use crate::{
    EvmTypesHost,
    bytecode::{CODE_CHUNK_SIZE, CodeChunk},
    evm::CodeChunkLoad,
    trustme,
    utils::WordExt,
    version::GasId,
};
use alloc::vec::Vec;
use alloy_primitives::Address;

/// Code ownership and execution position for one interpreter frame.
///
/// The frame's initial chunk is borrowed from its message, so starting a frame does not write the
/// shared refcount of read-mostly code. A chunk switch replaces it with an owned chunk.
#[derive(Debug)]
pub(super) struct InterpreterCode<'frame> {
    active: ActiveChunk<'frame>,
    // Borrows immutable allocations of `active`. Accessors shorten the erased lifetime;
    // activation clears this view before replacing the active chunk.
    view: Option<BytecodeRef<'static>>,
    pub(super) pc: *const u8,
}

#[derive(Debug)]
enum ActiveChunk<'frame> {
    Borrowed(&'frame CodeChunk),
    Owned(CodeChunk),
}

impl Default for InterpreterCode<'_> {
    fn default() -> Self {
        Self::from_active(ActiveChunk::Owned(CodeChunk::default()))
    }
}

impl<'frame> InterpreterCode<'frame> {
    pub(super) fn new(code: &'frame CodeChunk) -> Self {
        Self::from_active(ActiveChunk::Borrowed(code))
    }

    fn from_active(active: ActiveChunk<'frame>) -> Self {
        let pc = active.get().as_bytecode().bytes_slice().as_ptr();
        Self { active, pc, view: None }
    }

    pub(super) const fn active(&self) -> &CodeChunk {
        self.active.get()
    }

    pub(super) const fn is_account(&self) -> bool {
        self.active().is_account()
    }

    pub(super) fn view(&self) -> BytecodeRef<'_> {
        self.view.unwrap_or_else(|| BytecodeRef::new(self.active().as_bytecode()))
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
            let bytecode = unsafe { trustme::decouple_lt(self.active().as_bytecode()) };
            self.view = Some(BytecodeRef::new(bytecode));
        }
    }

    pub(super) fn activate(&mut self, chunk: CodeChunk) {
        self.view = None;
        self.active = ActiveChunk::Owned(chunk);
        self.pc = self.active().as_bytecode().bytes_slice().as_ptr();
        self.prepare();
    }

    pub(super) fn pc(&self) -> usize {
        self.global_pc(Pc::new(self.pc))
    }

    pub(super) fn set_pc(&mut self, pc: usize) {
        let local = pc
            .checked_sub(self.active().index() as usize * CODE_CHUNK_SIZE)
            .expect("PC is outside the active chunk");
        if self.active().is_chunked() {
            assert!(local < self.active().payload_len(), "PC is outside the logical payload");
        }
        let bytes = self.active().as_bytecode().bytes_slice();
        assert!(local < bytes.len());
        // SAFETY: The local offset is inside the active allocation.
        self.pc = unsafe { bytes.as_ptr().add(local) };
    }

    pub(super) fn global_pc(&self, pc: Pc) -> usize {
        // SAFETY: Every execution PC points into the active chunk.
        let local =
            unsafe { pc.as_ptr().offset_from(self.active().as_bytecode().bytes_slice().as_ptr()) };
        local as usize + self.active().index() as usize * CODE_CHUNK_SIZE
    }
}

impl ActiveChunk<'_> {
    const fn get(&self) -> &CodeChunk {
        match self {
            Self::Borrowed(chunk) => chunk,
            Self::Owned(chunk) => chunk,
        }
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

    /// Returns the active account buffer; only valid for account execution.
    pub(crate) fn active_chunk(&self) -> &CodeChunk {
        let chunk = self.0.code.active();
        assert!(chunk.is_account(), "account code required");
        chunk
    }

    /// Loads an account execution buffer and its prior coldness in one host access.
    pub(crate) fn execution_code_chunk(
        &mut self,
        address: &Address,
        index: u32,
        skip_cold: bool,
        expected_code_size: Option<u32>,
    ) -> Result<CodeChunkLoad> {
        let loaded = self
            .host()
            .load_code_chunk(address, index, skip_cold)
            .map_err(|error| self.fail(error))?
            .ok_or_else(|| {
                self.fail(crate::DatabaseError::new(
                    MissingRuntimeChunk { address: *address, index },
                    true,
                ))
            })?;
        if loaded.chunk.index() != index
            || expected_code_size.is_some_and(|code_size| {
                loaded.chunk.is_chunked()
                    && loaded.chunk.validate_context(code_size, index).is_err()
            })
        {
            let hash = loaded.chunk.as_bytecode().hash_slow();
            return Err(self
                .fail(crate::DatabaseError::new(crate::CodeChunkLoadError::Invalid(hash), true)));
        }
        Ok(loaded)
    }

    /// Replaces the owner before rebuilding its borrowed view. Callers reset their PC next.
    pub(crate) fn activate_code_chunk(&mut self, chunk: CodeChunk) {
        self.0.code.activate(chunk);
    }

    /// Whether an executed instruction ended outside the active logical payload.
    #[inline(always)]
    pub(crate) fn needs_code_chunk_advance(&self, pc: Pc) -> bool {
        let active = self.0.code.active();
        active.is_chunked() && self.bytecode().pc_offset(pc) >= active.payload_len()
    }

    /// Completes fallthrough at the logical payload boundary without executing padding.
    pub(crate) fn advance_code_chunk(&mut self, pc: &mut Pc) -> Result {
        let active = self.0.code.active();
        if !active.is_chunked() {
            return Ok(());
        }
        let local = self.bytecode().pc_offset(*pc);
        let payload_len = active.payload_len();
        if local < payload_len {
            return Ok(());
        }
        let next_pc = active.index() as usize * CODE_CHUNK_SIZE + local;
        if next_pc >= active.code_size() {
            return Err(InstrStop::Stop);
        }
        let index = active.index() + 1;
        let address = self.message().code_address;
        let warm = self.gas_params().get(GasId::WarmCodeChunkCost);
        let cold = self.gas_params().get(GasId::ColdCodeChunkCost);
        self.0.gas.spend(warm)?;
        let cold_additional = cold.saturating_sub(warm);
        let code_size = self.code_size() as u32;
        let loaded = self.execution_code_chunk(
            &address,
            index,
            self.0.gas.remaining() < cold_additional,
            Some(code_size),
        )?;
        if loaded.is_cold {
            self.0.gas.spend(cold_additional)?;
        }
        let offset = local - payload_len;
        if !self.active_chunk().continues_into(&loaded.chunk, offset) {
            let hash = loaded.chunk.as_bytecode().hash_slow();
            return Err(self
                .fail(crate::DatabaseError::new(crate::CodeChunkLoadError::Invalid(hash), true)));
        }
        self.activate_code_chunk(loaded.chunk);
        unsafe { pc.set_unchecked(self.bytecode(), offset) };
        Ok(())
    }

    /// Pays the warm charge first, skipping a cold load if its premium is unaffordable.
    pub(crate) fn load_and_charge_code_chunk(
        &mut self,
        gas: &mut Gas,
        address: &Address,
        index: u32,
        expected_code_size: Option<u32>,
    ) -> Result<CodeChunk> {
        let warm = self.gas_params().get(GasId::WarmCodeChunkCost);
        let cold = self.gas_params().get(GasId::ColdCodeChunkCost);
        gas.spend(warm)?;
        let cold_additional = cold.saturating_sub(warm);
        let loaded = self.execution_code_chunk(
            address,
            index,
            gas.remaining() < cold_additional,
            expected_code_size,
        )?;
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
        let source = source.to_usize_saturated();
        if source >= size {
            return Ok(Vec::new());
        }
        let end = source.saturating_add(len).min(size);
        let (first, last) = if multi {
            ((source / CODE_CHUNK_SIZE) as u32, ((end - 1) / CODE_CHUNK_SIZE) as u32)
        } else {
            (0, 0)
        };
        let mut bytes = Vec::with_capacity(end - source);
        let mut previous: Option<CodeChunk> = None;
        for index in first..=last {
            let chunk =
                self.load_and_charge_code_chunk(gas, &code_owner, index, u32::try_from(size).ok())?;
            if let Some(previous) = &previous
                && !previous.continues_into(&chunk, chunk.start_offset())
            {
                let hash = chunk.as_bytecode().hash_slow();
                return Err(self.fail(crate::DatabaseError::new(
                    crate::CodeChunkLoadError::Invalid(hash),
                    true,
                )));
            }
            let base = if multi { index as usize * CODE_CHUNK_SIZE } else { 0 };
            let from = source.saturating_sub(base).min(chunk.payload_len());
            let to = end.saturating_sub(base).min(chunk.payload_len());
            if from < to {
                bytes.extend_from_slice(&chunk.bytes()[from..to]);
            }
            previous = Some(chunk);
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
