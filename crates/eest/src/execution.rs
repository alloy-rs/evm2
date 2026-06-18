use evm2::{BaseEvmTypes, Evm};
#[cfg(feature = "jit")]
use evm2_jit_runtime::{evm2_evm::JitInterpreterRunner, runtime::JitBackend};
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
}

impl ExecutionResources {
    #[cfg(feature = "jit")]
    pub(crate) fn new(mode: ExecutionMode) -> Result<Self, ExecutionResourceError> {
        let backend = match mode {
            ExecutionMode::Interpreter => None,
            ExecutionMode::Jit | ExecutionMode::Aot => {
                let aot = mode == ExecutionMode::Aot;
                Some(crate::jit::make_backend(aot).map_err(ExecutionResourceError)?)
            }
        };
        Ok(Self { backend })
    }

    #[cfg(not(feature = "jit"))]
    pub(crate) const fn new(mode: ExecutionMode) -> Result<Self, ExecutionResourceError> {
        let _ = mode;
        Ok(Self {})
    }

    #[cfg(feature = "jit")]
    #[inline]
    pub(crate) fn configure_evm(&self, _evm: &mut Evm<BaseEvmTypes>) {
        if let Some(backend) = &self.backend {
            _evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        }
    }

    #[cfg(not(feature = "jit"))]
    #[inline]
    pub(crate) const fn configure_evm(&self, _evm: &mut Evm<BaseEvmTypes>) {
        let _ = self;
    }
}
