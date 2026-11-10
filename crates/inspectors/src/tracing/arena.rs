use super::types::{CallTrace, CallTraceNode, TraceMemberOrder};
use alloc::vec::Vec;
use alloy_primitives::Address;

/// An arena of recorded traces.
///
/// This type will be populated via the [TracingInspector](super::TracingInspector).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallTraceArena {
    /// The arena of recorded trace nodes
    pub(crate) arena: Vec<CallTraceNode>,
    /// Whether the root is synthetic, holding the top-level calls of a batch.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) nested: bool,
}

impl Default for CallTraceArena {
    fn default() -> Self {
        let mut this = Self { arena: Vec::with_capacity(8), nested: false };
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
        self.nested = false;
    }

    /// Returns __all__ addresses in the recorded traces, that is addresses of the trace and the
    /// caller address.
    pub fn trace_addresses(&self) -> impl Iterator<Item = Address> + '_ {
        self.nodes().iter().flat_map(|node| [node.trace.address, node.trace.caller].into_iter())
    }

    /// Pushes a new trace into the arena as a child of `parent`, returning the trace ID
    ///
    /// This appends a new trace to the arena, and also inserts a new entry in the node's parent
    /// node children set, one level below the parent.
    ///
    /// A trace without a parent at depth 0 fills the root node, or joins the earlier top-level
    /// calls under a synthetic root when the transaction runs several ([`Self::nest_root`]). A
    /// trace without a parent at a greater depth is attached to the root node instead, which stays
    /// an empty placeholder, as in revm-inspectors: top-level messages that start below an
    /// implicit parent, such as the calls of a batch transaction, then share one root.
    pub(crate) fn push_trace(&mut self, parent: Option<usize>, mut new_trace: CallTrace) -> usize {
        let parent = match parent {
            Some(parent) => parent,
            None if self.nested || new_trace.depth > 0 => 0,
            // The entry node, just update it.
            None if self.arena[0].trace.status.is_none() => {
                self.arena[0].trace = new_trace;
                return 0;
            }
            None => {
                self.nest_root();
                0
            }
        };
        if self.nested {
            // The synthetic root shifted every call a level down.
            new_trace.depth = self.arena[parent].trace.depth + 1;
        }
        debug_assert!(
            parent == 0 || self.arena[parent].trace.depth + 1 == new_trace.depth,
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

    /// Moves the recorded root and its subtree down a level under a synthetic root at index 0,
    /// which keeps the root's depth, caller and gas limit.
    fn nest_root(&mut self) {
        let CallTrace { depth, caller, gas_limit, .. } = self.arena[0].trace;
        // Shift every node by one so parents still precede their children.
        for node in &mut self.arena {
            node.idx += 1;
            node.trace.depth += 1;
            node.parent = Some(node.parent.map_or(0, |parent| parent + 1));
            for child in &mut node.children {
                *child += 1;
            }
        }
        let trace = CallTrace { depth, caller, gas_limit, ..Default::default() };
        let root = CallTraceNode {
            children: alloc::vec![1],
            ordering: alloc::vec![TraceMemberOrder::Call(0)],
            trace,
            ..Default::default()
        };
        self.arena.insert(0, root);
        self.settle_batch_call(1);
        self.nested = true;
    }

    /// Folds a finished top-level call into the synthetic root: the gas of every call, the
    /// outcome of the last one.
    pub(crate) fn settle_batch_call(&mut self, idx: usize) {
        let (root, calls) = self.arena.split_at_mut(1);
        let (root, call) = (&mut root[0].trace, &calls[idx - 1].trace);
        root.gas_used += call.gas_used;
        root.success = call.success;
        root.status = call.status;
        root.output = call.output.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parentless_traces_above_depth_zero_share_an_empty_root() {
        let mut arena = CallTraceArena::default();
        let first = arena.push_trace(None, CallTrace { depth: 1, ..Default::default() });
        arena.push_trace(Some(first), CallTrace { depth: 2, ..Default::default() });
        let second = arena.push_trace(None, CallTrace { depth: 1, ..Default::default() });

        let nodes = arena.nodes();
        assert_eq!(nodes[0].trace, CallTrace::default());
        assert_eq!(nodes[0].children, [first, second]);
        assert_eq!(nodes[first].parent, Some(0));
        assert_eq!(nodes[second].parent, Some(0));
    }

    #[test]
    fn parentless_trace_at_depth_zero_replaces_the_root() {
        let mut arena = CallTraceArena::default();
        let address = Address::repeat_byte(0x11);
        assert_eq!(arena.push_trace(None, CallTrace { address, ..Default::default() }), 0);
        assert_eq!(arena.nodes().len(), 1);
        assert_eq!(arena.nodes()[0].trace.address, address);
    }
}
