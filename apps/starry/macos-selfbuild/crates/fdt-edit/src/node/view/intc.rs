//! Interrupt controller node view specialization.

use core::ops::Deref;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use fdt_raw::Phandle;

use super::NodeView;
use crate::{NodeGeneric, NodeGenericMut, Property, ViewMutOp, ViewOp};

/// Interrupt reference, used to parse the `interrupts` property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterruptRef {
    /// Optional interrupt name from `interrupt-names`.
    pub name: Option<String>,
    /// Effective interrupt parent controller phandle.
    pub interrupt_parent: Phandle,
    /// Provider `#interrupt-cells` value used to parse the specifier.
    pub cells: u32,
    /// Raw interrupt specifier cells.
    pub specifier: Vec<u32>,
}

impl InterruptRef {
    /// Creates a named interrupt reference.
    pub fn with_name(
        name: Option<String>,
        interrupt_parent: Phandle,
        cells: u32,
        specifier: Vec<u32>,
    ) -> Self {
        Self {
            name,
            interrupt_parent,
            cells,
            specifier,
        }
    }
}

// ---------------------------------------------------------------------------
// IntcNodeView
// ---------------------------------------------------------------------------

/// Specialized view for interrupt controller nodes.
#[derive(Clone, Copy)]
pub struct IntcNodeView<'a> {
    pub(super) inner: NodeGeneric<'a>,
}

impl<'a> ViewOp<'a> for IntcNodeView<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> Deref for IntcNodeView<'a> {
    type Target = NodeGeneric<'a>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> IntcNodeView<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_interrupt_controller() {
            Some(Self {
                inner: NodeGeneric { inner: view },
            })
        } else {
            None
        }
    }

    /// Returns the `#interrupt-cells` property value.
    pub fn interrupt_cells(&self) -> Option<u32> {
        self.as_view().as_node().interrupt_cells()
    }

    /// Returns the `#address-cells` property value used by `interrupt-map`.
    pub fn interrupt_address_cells(&self) -> Option<u32> {
        self.as_view().as_node().address_cells()
    }

    /// This is always `true` for `IntcNodeView` (type-level guarantee).
    pub fn is_interrupt_controller(&self) -> bool {
        true
    }

    /// Returns all compatible strings as owned values.
    pub fn compatibles(&self) -> Vec<String> {
        self.as_view()
            .as_node()
            .compatibles()
            .map(|s| s.to_string())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// IntcNodeViewMut
// ---------------------------------------------------------------------------

/// Mutable view for interrupt controller nodes.
pub struct IntcNodeViewMut<'a> {
    pub(super) inner: NodeGenericMut<'a>,
}

impl<'a> ViewOp<'a> for IntcNodeViewMut<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> ViewMutOp<'a> for IntcNodeViewMut<'a> {
    fn new(node: NodeGenericMut<'a>) -> Self {
        let mut s = Self { inner: node };
        let n = s.inner.inner.as_node_mut();
        n.set_property(Property::new("interrupt-controller", Vec::new()));
        s
    }
}

impl<'a> Deref for IntcNodeViewMut<'a> {
    type Target = NodeGenericMut<'a>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> IntcNodeViewMut<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_interrupt_controller() {
            Some(Self {
                inner: NodeGenericMut { inner: view },
            })
        } else {
            None
        }
    }
}
