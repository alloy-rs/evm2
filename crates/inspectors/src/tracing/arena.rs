use super::types::{CallTrace, CallTraceNode, TraceMemberOrder};
use alloc::{vec, vec::Vec};
use alloy_primitives::Address;
use evm2::interpreter::InstrStop;

/// An arena of recorded traces.
///
/// This type will be populated via the [TracingInspector](super::TracingInspector).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallTraceArena {
    /// The arena of recorded trace nodes
    pub(crate) arena: Vec<CallTraceNode>,
    /// What the root holds.
    #[cfg_attr(feature = "serde", serde(skip))]
    root: Root,
}

impl Default for CallTraceArena {
    fn default() -> Self {
        let mut this = Self { arena: Vec::with_capacity(8), root: Root::Call };
        this.clear();
        this
    }
}

impl CallTraceArena {
    /// Returns the nodes in the arena.
    #[inline]
    pub fn nodes(&self) -> &[CallTraceNode] {
        &self.arena
    }

    /// Returns a mutable reference to the nodes in the arena.
    #[inline]
    pub const fn nodes_mut(&mut self) -> &mut Vec<CallTraceNode> {
        &mut self.arena
    }

    /// Consumes the arena and returns the nodes.
    #[inline]
    pub fn into_nodes(self) -> Vec<CallTraceNode> {
        self.arena
    }

    /// Clears the arena
    ///
    /// Note that this method has no effect on the allocated capacity of the arena.
    pub fn clear(&mut self) {
        self.arena.clear();
        self.arena.push(Default::default());
        self.root = Root::Call;
    }

    /// Marks the transaction as a batch, so that its first top-level call already sits under a
    /// synthetic root, see [`Self::nest_root`].
    ///
    /// Must be called before the first top-level call.
    pub(crate) fn start_batch(&mut self) {
        debug_assert!(self.arena.len() == 1, "a batch starts before its first call");
        self.root = Root::PendingBatch;
    }

    /// Returns __all__ addresses in the recorded traces, that is addresses of the trace and the
    /// caller address.
    pub fn trace_addresses(&self) -> impl Iterator<Item = Address> + '_ {
        self.nodes().iter().flat_map(|node| [node.trace.address, node.trace.caller].into_iter())
    }

    /// Pushes a new trace into the arena as a child of `parent`, returning the trace ID
    ///
    /// This appends a new trace to the arena, and also inserts a new entry in the node's parent
    /// node children set.
    ///
    /// Without a parent, the first top-level call of a transaction fills the root, and moves under
    /// a synthetic root right away if the transaction is a batch, see [`Self::start_batch`]. A
    /// later top-level call moves the recorded root under a synthetic root, see
    /// [`Self::nest_root`], and joins it there.
    pub(crate) fn push_trace(&mut self, parent: Option<usize>, mut new_trace: CallTrace) -> usize {
        let parent = match parent {
            Some(parent) => parent,
            // The placeholder root, just fill it in.
            None if self.root != Root::Batch && self.arena[0].trace.status.is_none() => {
                self.arena[0].trace = new_trace;
                if self.root == Root::Call {
                    return 0;
                }
                self.nest_root();
                return 1;
            }
            None => {
                if self.root == Root::Call {
                    self.nest_root();
                }
                // The transaction's own calls are never left out as precompile calls.
                new_trace.maybe_precompile = new_trace.maybe_precompile.map(|_| false);
                0
            }
        };
        // Below a synthetic root, every call sits one level deeper than the EVM runs it.
        new_trace.depth += usize::from(self.root == Root::Batch);
        debug_assert_eq!(
            self.arena[parent].trace.depth + 1,
            new_trace.depth,
            "parent must be exactly one level above the new trace"
        );

        let idx = self.arena.len();
        self.arena.push(CallTraceNode {
            parent: Some(parent),
            trace: new_trace,
            idx,
            ..Default::default()
        });

        // Also track the child in the parent node.
        let parent = &mut self.arena[parent];
        let trace_location = parent.children.len();
        parent.ordering.push(TraceMemberOrder::Call(trace_location));
        parent.children.push(idx);

        idx
    }

    /// Records the outcome of a finished top-level call of a batch on the synthetic root: the gas
    /// of every call, and the status and output of the last one, since a failed call ends the
    /// batch.
    ///
    /// Does nothing unless the arena holds a batch and `idx` is one of its top-level calls.
    pub(crate) fn settle_batch_call(&mut self, idx: usize) {
        if self.root != Root::Batch || self.arena[idx].parent != Some(0) {
            return;
        }
        let (root, calls) = self.arena.split_at_mut(1);
        let (root, call) = (&mut root[0].trace, &calls[idx - 1].trace);
        root.gas_used += call.gas_used;
        copy_outcome(root, call);
    }

    /// Moves the recorded root and its subtree one level down under a synthetic root at index 0,
    /// which takes the depth, caller, gas limit and outcome of the first call.
    fn nest_root(&mut self) {
        let depth = self.arena[0].trace.depth;
        // Shift every index by one so parents still precede their children.
        for node in &mut self.arena {
            node.idx += 1;
            node.trace.depth += 1;
            node.parent = Some(node.parent.map_or(0, |parent| parent + 1));
            for child in &mut node.children {
                *child += 1;
            }
        }
        let first = &mut self.arena[0].trace;
        first.maybe_precompile = first.maybe_precompile.map(|_| false);
        let mut root = CallTrace {
            depth,
            caller: first.caller,
            gas_limit: first.gas_limit,
            gas_used: first.gas_used,
            ..Default::default()
        };
        copy_outcome(&mut root, first);
        self.arena.insert(
            0,
            CallTraceNode {
                children: vec![1],
                ordering: vec![TraceMemberOrder::Call(0)],
                trace: root,
                ..Default::default()
            },
        );
        self.root = Root::Batch;
    }
}

/// What the root of a [`CallTraceArena`] holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Root {
    /// The transaction's top-level call, or a placeholder until it starts.
    #[default]
    Call,
    /// A placeholder until the first top-level call of a batch starts, see
    /// [`CallTraceArena::start_batch`].
    PendingBatch,
    /// A synthetic call holding the transaction's top-level calls.
    Batch,
}

/// Copies the status and output of a top-level call onto the synthetic root of a batch, which
/// destroys nothing itself.
fn copy_outcome(root: &mut CallTrace, call: &CallTrace) {
    root.status = call.status.map(|status| match status {
        InstrStop::SelfDestruct => InstrStop::Stop,
        status => status,
    });
    root.success = call.success;
    root.output = call.output.clone();
}
