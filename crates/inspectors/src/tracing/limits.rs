//! Recording limits and internal byte accounting.

use evm2::{EvmTypes, ExecutionError, interpreter::Interpreter};

/// Behavior when a trace recording reaches its byte budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceLimitBehavior {
    /// Continue execution but omit byte buffers that would exceed the limit.
    /// The resulting trace can contain empty fields. Check
    /// [`TracingInspector::limit_exceeded`](super::TracingInspector::limit_exceeded)
    /// to detect this.
    #[default]
    Skip,
    /// Abort execution with a fatal evm2 error when a byte buffer would exceed the limit.
    Halt,
}

/// Limits applied when recording trace data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TraceLimits {
    /// Maximum cumulative byte-buffer lengths between resets. `None` means unlimited.
    ///
    /// Counts retained child call inputs, copied memory snapshots, immediate bytes, and memory
    /// deltas. Shared buffer clones other than child inputs, stack/trace vector elements, and
    /// allocator overhead are not counted. Checked before retaining or copying each buffer.
    pub max_recorded_bytes: Option<usize>,
    /// What to do when a recording would exceed `max_recorded_bytes`.
    pub behavior: TraceLimitBehavior,
}

impl TraceLimits {
    /// Sets the recorded-byte limit. `None` means unlimited.
    pub const fn set_max_recorded_bytes(mut self, max_recorded_bytes: Option<usize>) -> Self {
        self.max_recorded_bytes = max_recorded_bytes;
        self
    }

    /// Sets the behavior when a recording would exceed the budget.
    pub const fn set_behavior(mut self, behavior: TraceLimitBehavior) -> Self {
        self.behavior = behavior;
        self
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceBudget {
    pub(crate) limits: TraceLimits,
    pub(crate) recorded: usize,
    pub(crate) exceeded: bool,
}

impl TraceBudget {
    pub(crate) fn reserve(&mut self, bytes: usize) -> bool {
        if self.exceeded
            || self.limits.max_recorded_bytes.is_some_and(|limit| {
                self.recorded.checked_add(bytes).is_none_or(|total| total > limit)
            })
        {
            self.exceeded = true;
            return false;
        }
        self.recorded += bytes;
        true
    }

    pub(crate) const fn exceeded(&self) -> bool {
        self.exceeded
    }

    pub(crate) fn halt<T: EvmTypes>(&self, interp: &mut Interpreter<'_, '_, T>, in_step: bool) {
        if self.exceeded && self.limits.behavior == TraceLimitBehavior::Halt {
            let stop =
                interp.fail(ExecutionError::Fatal("trace recorded byte limit exceeded".into()));
            if in_step {
                interp.set_stop(stop);
            }
        }
    }
}
