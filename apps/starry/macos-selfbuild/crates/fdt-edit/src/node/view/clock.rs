//! Clock node view specialization.

use core::ops::Deref;

use alloc::{borrow::ToOwned, string::String, vec::Vec};
use fdt_raw::Phandle;

use super::NodeView;
use crate::{NodeGeneric, NodeGenericMut, Property, ViewMutOp, ViewOp};

// ---------------------------------------------------------------------------
// Clock types
// ---------------------------------------------------------------------------

/// Clock provider type.
#[derive(Clone, Debug, PartialEq)]
pub enum ClockType {
    /// Fixed clock
    Fixed(FixedClock),
    /// Normal clock provider
    Normal,
}

/// Fixed clock provider.
///
/// Represents a fixed-rate clock that always operates at a constant frequency.
#[derive(Clone, Debug, PartialEq)]
pub struct FixedClock {
    /// Optional name for the clock
    pub name: Option<String>,
    /// Clock frequency in Hz
    pub frequency: u32,
    /// Clock accuracy in ppb (parts per billion)
    pub accuracy: Option<u32>,
}

/// Clock reference, used to parse clocks property.
///
/// According to the device tree specification, the clocks property format is:
/// `clocks = <&clock_provider specifier [specifier ...]> [<&clock_provider2 ...>]`
///
/// Each clock reference consists of a phandle and several specifier cells,
/// the number of specifiers is determined by the target clock provider's `#clock-cells` property.
#[derive(Clone, Debug)]
pub struct ClockRef {
    /// Clock name, from clock-names property
    pub name: Option<String>,
    /// Phandle of the clock provider
    pub phandle: Phandle,
    /// #clock-cells value of the provider
    pub cells: u32,
    /// Clock selector (specifier), usually the first value is used to select clock output
    /// Length is determined by provider's #clock-cells
    pub specifier: Vec<u32>,
}

impl ClockRef {
    /// Create a new clock reference
    pub fn new(phandle: Phandle, cells: u32, specifier: Vec<u32>) -> Self {
        Self {
            name: None,
            phandle,
            cells,
            specifier,
        }
    }

    /// Create a named clock reference
    pub fn with_name(
        name: Option<String>,
        phandle: Phandle,
        cells: u32,
        specifier: Vec<u32>,
    ) -> Self {
        Self {
            name,
            phandle,
            cells,
            specifier,
        }
    }

    /// Get the first value of the selector (usually used to select clock output)
    ///
    /// Only returns a selector value when `cells > 0`,
    /// because providers with `#clock-cells = 0` don't need a selector.
    pub fn select(&self) -> Option<u32> {
        if self.cells > 0 {
            self.specifier.first().copied()
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// ClockNodeView
// ---------------------------------------------------------------------------

/// Specialized view for clock provider nodes.
#[derive(Clone, Copy)]
pub struct ClockNodeView<'a> {
    pub(super) inner: NodeGeneric<'a>,
}

impl<'a> Deref for ClockNodeView<'a> {
    type Target = NodeGeneric<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> ViewOp<'a> for ClockNodeView<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> ClockNodeView<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_clock() {
            Some(Self {
                inner: NodeGeneric { inner: view },
            })
        } else {
            None
        }
    }

    /// Get the value of the `#clock-cells` property.
    pub fn clock_cells(&self) -> u32 {
        self.as_view()
            .as_node()
            .get_property("#clock-cells")
            .and_then(|prop| prop.get_u32())
            .unwrap_or(0)
    }

    /// Get clock output names from the `clock-output-names` property.
    pub fn clock_output_names(&self) -> Vec<String> {
        self.as_view()
            .as_node()
            .get_property("clock-output-names")
            .map(|prop| prop.as_str_iter().map(|s| s.to_owned()).collect())
            .unwrap_or_default()
    }

    /// Get clock output name by index.
    pub fn output_name(&self, index: usize) -> Option<String> {
        self.clock_output_names().get(index).cloned()
    }

    /// Get the clock type (Fixed or Normal).
    pub fn clock_type(&self) -> ClockType {
        let node = self.as_view().as_node();

        // Check if this is a fixed-clock
        let is_fixed = node
            .get_property("compatible")
            .and_then(|prop| prop.as_str_iter().find(|&c| c == "fixed-clock"))
            .is_some();

        if is_fixed {
            let frequency = node
                .get_property("clock-frequency")
                .and_then(|prop| prop.get_u32())
                .unwrap_or(0);

            let accuracy = node
                .get_property("clock-accuracy")
                .and_then(|prop| prop.get_u32());

            let name = self.clock_output_names().first().cloned();

            ClockType::Fixed(FixedClock {
                name,
                frequency,
                accuracy,
            })
        } else {
            ClockType::Normal
        }
    }
}

// ---------------------------------------------------------------------------
// ClockNodeViewMut
// ---------------------------------------------------------------------------

/// Mutable view for clock provider nodes.
pub struct ClockNodeViewMut<'a> {
    pub(super) inner: NodeGenericMut<'a>,
}

impl<'a> Deref for ClockNodeViewMut<'a> {
    type Target = NodeGenericMut<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> ViewOp<'a> for ClockNodeViewMut<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> ViewMutOp<'a> for ClockNodeViewMut<'a> {
    fn new(node: NodeGenericMut<'a>) -> Self {
        let mut s = Self { inner: node };
        let n = s.inner.inner.as_node_mut();
        // Set #clock-cells property (default to 0)
        n.set_property(Property::new("#clock-cells", (0u32).to_be_bytes().to_vec()));
        s
    }
}

impl<'a> ClockNodeViewMut<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_clock() {
            Some(Self {
                inner: NodeGenericMut { inner: view },
            })
        } else {
            None
        }
    }
}
