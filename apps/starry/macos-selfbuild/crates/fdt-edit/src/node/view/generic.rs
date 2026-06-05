//! Generic node view specialization.

use alloc::{string::String, vec::Vec};
use fdt_raw::{Phandle, RegInfo};

use super::NodeView;
use crate::{ClockRef, InterruptRef, Node, NodeId, RegFixed, ViewMutOp, ViewOp};

// ---------------------------------------------------------------------------
// GenericNodeView
// ---------------------------------------------------------------------------

/// A generic node view with no extra specialization.
#[derive(Clone, Copy)]
pub struct NodeGeneric<'a> {
    pub(super) inner: NodeView<'a>,
}

impl<'a> NodeGeneric<'a> {
    pub fn id(&self) -> NodeId {
        self.inner.id()
    }

    pub fn path(&self) -> String {
        self.inner.path()
    }

    pub fn regs(&self) -> Vec<RegFixed> {
        self.inner.regs()
    }

    /// Returns the effective `interrupt-parent`, inheriting from ancestors.
    pub fn interrupt_parent(&self) -> Option<Phandle> {
        self.inner.interrupt_parent()
    }

    /// Parses the `clocks` property into clock references.
    pub fn clocks(&self) -> Vec<ClockRef> {
        self.inner.clocks()
    }

    /// Parses the `interrupts` property into interrupt references.
    pub fn interrupts(&self) -> Vec<InterruptRef> {
        self.inner.interrupts()
    }
}

impl<'a> ViewOp<'a> for NodeGeneric<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner
    }
}

// ---------------------------------------------------------------------------
// GenericNodeViewMut
// ---------------------------------------------------------------------------

/// Mutable view for generic nodes.
pub struct NodeGenericMut<'a> {
    pub(super) inner: NodeView<'a>,
}

impl<'a> ViewOp<'a> for NodeGenericMut<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner
    }
}

impl<'a> ViewMutOp<'a> for NodeGenericMut<'a> {
    fn new(node: NodeGenericMut<'a>) -> Self {
        Self { inner: node.inner }
    }
}

impl<'a> NodeGenericMut<'a> {
    pub fn id(&self) -> NodeId {
        self.inner.id()
    }

    pub fn path(&self) -> String {
        self.inner.path()
    }

    pub fn set_regs(&mut self, regs: &[RegInfo]) {
        self.inner.set_regs(regs);
    }

    pub fn add_child_generic(&mut self, name: &str) -> NodeGenericMut<'a> {
        let node = Node::new(name);
        let new_id = self.inner.fdt_mut().add_node(self.inner.id(), node);
        let new_view = NodeView::new(self.inner.fdt(), new_id);
        NodeGenericMut { inner: new_view }
    }

    pub(crate) fn add_child<T: ViewMutOp<'a>>(&mut self, name: &str) -> T {
        let generic_child = self.add_child_generic(name);
        T::new(generic_child)
    }
}
