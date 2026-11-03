use evm2::{BaseEvmTypes, Evm};
#[cfg(feature = "jit")]
use evm2_jit_runtime::{evm2_evm::JitInterpreterRunner, runtime::JitBackend};
#[cfg(feature = "superinstr")]
use evm2_superinstr::{SuperInstrBackend, SuperInstrConfig, SuperInstrRunner};
#[cfg(any(feature = "jit", feature = "superinstr"))]
use std::sync::OnceLock;
use std::{error::Error, fmt};

/// EEST execution backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Run through the evm2 interpreter.
    #[default]
    Interpreter,
    /// Run through the evm2 JIT runtime, falling back to the interpreter for unsupported code.
    #[cfg(feature = "jit")]
    Jit,
    /// Run through the evm2 AOT runtime, falling back to the interpreter for unsupported code.
    #[cfg(feature = "jit")]
    Aot,
    /// Run through the superinstruction backend, falling back to the interpreter for unsupported
    /// code.
    #[cfg(feature = "superinstr")]
    SuperInstr,
}

#[cfg(feature = "jit")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompiledMode {
    Jit,
    Aot,
}

#[cfg(feature = "jit")]
impl CompiledMode {
    #[inline]
    pub(crate) const fn execution_mode(self) -> ExecutionMode {
        match self {
            Self::Jit => ExecutionMode::Jit,
            Self::Aot => ExecutionMode::Aot,
        }
    }

    #[inline]
    pub(crate) const fn suffix(self) -> &'static str {
        match self {
            Self::Jit => "jit",
            Self::Aot => "aot",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionResourceError(String);

impl fmt::Display for ExecutionResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "jit runtime error: {}", self.0)
    }
}

impl Error for ExecutionResourceError {}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionResources {
    #[cfg(feature = "jit")]
    backend: Option<JitBackend>,
    #[cfg(feature = "superinstr")]
    superinstr: Option<SuperInstrBackend>,
}

impl ExecutionResources {
    pub(crate) fn new(mode: ExecutionMode) -> Result<Self, ExecutionResourceError> {
        #[cfg(feature = "jit")]
        let backend = match mode {
            ExecutionMode::Jit if shared_jit_requested() => Some(shared_jit()?),
            ExecutionMode::Jit | ExecutionMode::Aot => {
                let aot = mode == ExecutionMode::Aot;
                Some(crate::jit::make_backend(aot).map_err(ExecutionResourceError)?)
            }
            #[allow(unreachable_patterns)]
            _ => None,
        };
        #[cfg(feature = "superinstr")]
        let superinstr = (mode == ExecutionMode::SuperInstr).then(shared_superinstr);
        let _ = mode;
        Ok(Self {
            #[cfg(feature = "jit")]
            backend,
            #[cfg(feature = "superinstr")]
            superinstr,
        })
    }

    #[inline]
    pub(crate) fn configure_evm(&self, evm: &mut Evm<'_, BaseEvmTypes>) {
        #[cfg(feature = "jit")]
        if let Some(backend) = &self.backend {
            evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        }
        #[cfg(feature = "superinstr")]
        if let Some(backend) = &self.superinstr {
            evm.set_interpreter_runner(SuperInstrRunner::new(backend.clone()));
        }
        let _ = evm;
    }

    /// Fails if a compilation failed or a frame fell back to the interpreter, since the test
    /// would then pass without exercising compiled code.
    #[cfg(feature = "jit")]
    pub(crate) fn check_compilations(&self) -> Result<(), ExecutionResourceError> {
        let Some(backend) = &self.backend else { return Ok(()) };
        if shared_jit_requested() {
            // A shared backend accumulates every run's fallbacks, and benchmarks accept them.
            return Ok(());
        }
        let stats = backend.stats();
        let (failed, fallbacks) = (stats.compilations_failed, stats.interpreter_fallbacks);
        if failed == 0 && fallbacks == 0 {
            return Ok(());
        }
        Err(ExecutionResourceError(format!(
            "{failed} compilation(s) failed and {fallbacks} frame(s) fell back to the interpreter \
             in this run; rerun with RUST_LOG=warn for the cause"
        )))
    }

    #[cfg(not(feature = "jit"))]
    #[inline]
    pub(crate) const fn check_compilations(&self) -> Result<(), ExecutionResourceError> {
        let _ = self;
        Ok(())
    }

    #[cfg(feature = "jit")]
    pub(crate) fn print_runtime_stats(&self) {
        if let Some(backend) = &self.backend {
            let stats = backend.stats();
            println!(
                "Runtime backend: {} hits, {} misses, {} resident",
                stats.lookup_hits, stats.lookup_misses, stats.resident_entries,
            );
        }
    }
}

/// Returns the process-wide superinstruction backend, so translations persist across runs as they
/// would in a node.
#[cfg(feature = "superinstr")]
fn shared_superinstr() -> SuperInstrBackend {
    static BACKEND: OnceLock<SuperInstrBackend> = OnceLock::new();
    BACKEND.get_or_init(|| SuperInstrBackend::new(SuperInstrConfig::default())).clone()
}

/// Whether `EVM2_EEST_SHARED_JIT` asks JIT runs to share one process-wide backend with the
/// runtime's default optimization level, compiling every contract on first use. Benchmarks use it
/// to time compiled code without compiling it again on each run.
#[cfg(feature = "jit")]
fn shared_jit_requested() -> bool {
    static REQUESTED: OnceLock<bool> = OnceLock::new();
    *REQUESTED.get_or_init(|| std::env::var_os("EVM2_EEST_SHARED_JIT").is_some())
}

#[cfg(feature = "jit")]
fn shared_jit() -> Result<JitBackend, ExecutionResourceError> {
    use evm2_jit_runtime::runtime::{RuntimeConfig, RuntimeTuning};
    static BACKEND: OnceLock<Result<JitBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            JitBackend::new(RuntimeConfig {
                enabled: true,
                blocking: true,
                tuning: RuntimeTuning { jit_hot_threshold: 0, ..Default::default() },
                ..Default::default()
            })
            .map_err(|err| err.to_string())
        })
        .clone()
        .map_err(ExecutionResourceError)
}
