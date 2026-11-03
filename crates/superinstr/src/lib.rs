#![doc = include_str!("../README.md")]
#![cfg_attr(tco, feature(explicit_tail_calls), allow(incomplete_features))]
#![cfg_attr(tco, feature(rust_preserve_none_cc))]

use alloy_primitives::B256;
use dashmap::DashMap;
use evm2::{
    EvmTypesHost, ExecutionConfig, InterpreterRunner,
    interpreter::{Host, InstrStop, Interpreter},
};
use std::{
    any::{Any, TypeId},
    hash::BuildHasherDefault,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

mod exec;
mod program;
mod translate;

/// Configuration of a [`SuperInstrBackend`].
#[derive(Clone, Copy, Debug)]
pub struct SuperInstrConfig {
    /// Whether frames run on the backend.
    pub enabled: bool,
    /// Largest bytecode the backend translates. Larger code runs on the interpreter.
    pub max_bytecode_len: usize,
}

impl Default for SuperInstrConfig {
    fn default() -> Self {
        Self { enabled: true, max_bytecode_len: 0x10000 }
    }
}

/// Counters of a [`SuperInstrBackend`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SuperInstrStats {
    /// Contracts translated.
    pub translations: u64,
    /// Time spent translating and linking, in nanoseconds.
    pub translate_nanos: u64,
    /// Frames that handed off to the interpreter part-way.
    pub bails: u64,
    /// Programs in the cache.
    pub programs: usize,
}

/// Cache key of a translated program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProgramKey {
    code_hash: B256,
    types: TypeId,
    /// Address of the `'static` opcode table, which decides static gas and native instructions.
    opcodes: usize,
}

impl core::hash::Hash for ProgramKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        // The code hash is already uniformly distributed.
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.code_hash[..8]);
        state.write_u64(u64::from_le_bytes(bytes) ^ self.opcodes as u64);
    }
}

/// Hasher for [`ProgramKey`]s, which hash themselves into one well-mixed word.
#[derive(Clone, Copy, Debug, Default)]
struct KeyHasher(u64);

impl core::hash::Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = self.0.rotate_left(8) ^ byte as u64;
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 ^= value;
    }
}

#[derive(Debug)]
struct Inner {
    config: SuperInstrConfig,
    enabled: AtomicBool,
    programs: DashMap<ProgramKey, Arc<dyn Any + Send + Sync>, BuildHasherDefault<KeyHasher>>,
    translations: AtomicU64,
    translate_nanos: AtomicU64,
    bails: AtomicU64,
}

/// Shared superinstruction backend: its configuration, program cache and counters.
///
/// Clones share the same state.
#[derive(Clone, Debug)]
pub struct SuperInstrBackend {
    inner: Arc<Inner>,
}

impl SuperInstrBackend {
    /// Creates a backend.
    pub fn new(config: SuperInstrConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                enabled: AtomicBool::new(config.enabled),
                programs: DashMap::default(),
                translations: AtomicU64::new(0),
                translate_nanos: AtomicU64::new(0),
                bails: AtomicU64::new(0),
            }),
        }
    }

    /// Creates a disabled backend.
    pub fn disabled() -> Self {
        Self::new(SuperInstrConfig { enabled: false, ..Default::default() })
    }

    /// Returns whether frames run on the backend.
    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Relaxed)
    }

    /// Enables or disables the backend.
    pub fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Drops every cached program.
    pub fn clear(&self) {
        self.inner.programs.clear();
    }

    /// Returns the backend's counters.
    pub fn stats(&self) -> SuperInstrStats {
        let inner = &*self.inner;
        SuperInstrStats {
            translations: inner.translations.load(Ordering::Relaxed),
            translate_nanos: inner.translate_nanos.load(Ordering::Relaxed),
            bails: inner.bails.load(Ordering::Relaxed),
            programs: inner.programs.len(),
        }
    }

    /// Runs `interp`'s frame, translating its code on first use.
    ///
    /// Returns `None` when the backend is disabled or does not take the code, leaving the frame to
    /// the interpreter.
    pub fn run<'f, 'h, T: EvmTypesHost>(
        &self,
        interp: &mut Interpreter<'f, 'h, T>,
        host: &mut T::Host<'h>,
    ) -> Option<InstrStop> {
        if !self.enabled() {
            return None;
        }
        let program = self.program(interp, host.execution_config())?;
        let (stop, bailed) = exec::run(&program, interp, host);
        if bailed {
            self.inner.bails.fetch_add(1, Ordering::Relaxed);
        }
        Some(stop)
    }

    fn program<T: EvmTypesHost>(
        &self,
        interp: &Interpreter<'_, '_, T>,
        config: &ExecutionConfig<T>,
    ) -> Option<Arc<exec::Linked<T>>> {
        let code = interp.original_bytecode_slice();
        if code.is_empty() || code.len() > self.inner.config.max_bytecode_len {
            return None;
        }
        let key = ProgramKey {
            code_hash: interp.original_bytecode_hash(),
            types: TypeId::of::<T>(),
            opcodes: core::ptr::from_ref(config.opcode_config()) as usize,
        };
        if let Some(program) = self.inner.programs.get(&key) {
            return program.value().clone().downcast().ok();
        }
        let start = Instant::now();
        let bytecode = interp.bytecode();
        let program =
            translate::translate(code, |pc| bytecode.is_valid_jumpdest(pc), config.opcode_config());
        let linked = Arc::new(exec::link::<T>(program, config.opcode_config()));
        let inner = &*self.inner;
        inner.translate_nanos.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        inner.translations.fetch_add(1, Ordering::Relaxed);
        inner.programs.insert(key, linked.clone());
        Some(linked)
    }
}

/// Interpreter runner backed by a [`SuperInstrBackend`].
#[derive(Clone, Debug)]
pub struct SuperInstrRunner {
    backend: SuperInstrBackend,
}

impl SuperInstrRunner {
    /// Creates a runner from a backend.
    pub const fn new(backend: SuperInstrBackend) -> Self {
        Self { backend }
    }

    /// Returns the backend.
    pub const fn backend(&self) -> &SuperInstrBackend {
        &self.backend
    }
}

impl<T: EvmTypesHost> InterpreterRunner<T> for SuperInstrRunner {
    #[inline]
    fn run<'frame, 'host>(
        &self,
        interpreter: &mut Interpreter<'frame, 'host, T>,
        host: &mut T::Host<'host>,
    ) -> Option<InstrStop> {
        self.backend.run(interpreter, host)
    }
}

/// Translates and links `bytecode` for `config` without caching it, returning the number of
/// records. For tools and benchmarks that measure translation on its own.
#[doc(hidden)]
pub fn translate_records<T: EvmTypesHost>(
    bytecode: &evm2::bytecode::Bytecode,
    config: &ExecutionConfig<T>,
) -> usize {
    let jump_table = bytecode.legacy_jump_table();
    let program = translate::translate(
        bytecode.original_byte_slice(),
        |pc| jump_table.is_some_and(|table| table.is_valid(pc)),
        config.opcode_config(),
    );
    exec::link::<T>(program, config.opcode_config()).records()
}
