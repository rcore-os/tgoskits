//! Memory node view specialization.

use core::ops::Deref;

use alloc::vec::Vec;
use fdt_raw::MemoryRegion;

use super::NodeView;
use crate::{NodeGeneric, NodeGenericMut, Property, ViewMutOp, ViewOp};

// ---------------------------------------------------------------------------
// MemoryNodeView
// ---------------------------------------------------------------------------

/// Specialized view for memory nodes.
///
/// Provides methods for parsing `reg` into memory regions.
#[derive(Clone, Copy)]
pub struct MemoryNodeView<'a> {
    pub(super) inner: NodeGeneric<'a>,
}

impl<'a> Deref for MemoryNodeView<'a> {
    type Target = NodeGeneric<'a>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// Implement ViewOp for all specialized view types that have `inner: NodeView<'a>`
impl<'a> ViewOp<'a> for MemoryNodeView<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> MemoryNodeView<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_memory() {
            Some(Self {
                inner: NodeGeneric { inner: view },
            })
        } else {
            None
        }
    }

    /// Iterates over memory regions parsed from the `reg` property.
    ///
    /// Uses the parent node's `ranges` for address translation, converting
    /// bus addresses to CPU physical addresses.
    pub fn regions(&self) -> Vec<MemoryRegion> {
        // Use NodeView::regs() to get address-translated regions
        let regs = self.as_view().regs();
        regs.into_iter()
            .map(|r| MemoryRegion {
                address: r.address, // Use the CPU-translated address
                size: r.size.unwrap_or(0),
            })
            .collect()
    }

    /// Total size across all memory regions.
    pub fn total_size(&self) -> u64 {
        self.regions().iter().map(|r| r.size).sum()
    }
}

// ---------------------------------------------------------------------------
// MemoryNodeViewMut
// ---------------------------------------------------------------------------

/// Mutable view for memory nodes.
pub struct MemoryNodeViewMut<'a> {
    pub(super) inner: NodeGenericMut<'a>,
}

impl<'a> Deref for MemoryNodeViewMut<'a> {
    type Target = NodeGenericMut<'a>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> ViewOp<'a> for MemoryNodeViewMut<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> ViewMutOp<'a> for MemoryNodeViewMut<'a> {
    fn new(node: NodeGenericMut<'a>) -> Self {
        let mut s = Self { inner: node };
        let n = s.inner.inner.as_node_mut();
        n.set_property(Property::new("device_type", b"memory\0".to_vec()));
        s
    }
}

impl<'a> MemoryNodeViewMut<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_memory() {
            Some(Self {
                inner: NodeGenericMut { inner: view },
            })
        } else {
            None
        }
    }
}
