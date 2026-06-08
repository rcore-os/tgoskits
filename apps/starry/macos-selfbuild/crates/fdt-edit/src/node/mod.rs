use core::fmt::{Debug, Display};

use alloc::{collections::btree_map::BTreeMap, string::String, vec::Vec};
use fdt_raw::{Phandle, Status};

use crate::{NodeId, Property, RangesEntry};

pub(crate) mod view;

/// A mutable device tree node.
///
/// Represents a node in the device tree with a name, properties, and child node IDs.
/// Nodes are stored in a flat `BTreeMap<NodeId, Node>` within the `Fdt` struct,
/// and children are referenced by their `NodeId`.
#[derive(Clone)]
pub struct Node {
    /// Node name (without path)
    pub name: String,
    /// Property list (maintains original order)
    properties: Vec<Property>,
    /// Property name to index mapping (for fast lookup)
    prop_cache: BTreeMap<String, usize>,
    /// Child node IDs
    children: Vec<NodeId>,
    /// Child name to children-vec index mapping (for fast lookup).
    /// Note: the name key here needs to be resolved through the arena.
    name_cache: BTreeMap<String, usize>,
}

impl Node {
    /// Creates a new node with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            properties: Vec::new(),
            prop_cache: BTreeMap::new(),
            children: Vec::new(),
            name_cache: BTreeMap::new(),
        }
    }

    /// Returns the node's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns an iterator over the node's properties.
    pub fn properties(&self) -> &[Property] {
        &self.properties
    }

    /// Returns the child node IDs.
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    /// Adds a child node ID to this node.
    ///
    /// Updates the name cache for fast lookups.
    pub fn add_child(&mut self, name: &str, id: NodeId) {
        let index = self.children.len();
        self.name_cache.insert(name.into(), index);
        self.children.push(id);
    }

    /// Adds a property to this node.
    ///
    /// Updates the property cache for fast lookups.
    pub fn add_property(&mut self, prop: Property) {
        let name = prop.name.clone();
        let index = self.properties.len();
        self.prop_cache.insert(name, index);
        self.properties.push(prop);
    }

    /// Gets a child node ID by name.
    ///
    /// Uses the cache for fast lookup.
    pub fn get_child(&self, name: &str) -> Option<NodeId> {
        self.name_cache
            .get(name)
            .and_then(|&idx| self.children.get(idx).copied())
    }

    /// Removes a child node by name, returning its `NodeId`.
    ///
    /// Rebuilds the name cache after removal.
    pub fn remove_child(&mut self, name: &str) -> Option<NodeId> {
        let &idx = self.name_cache.get(name)?;
        if idx >= self.children.len() {
            return None;
        }
        let removed = self.children.remove(idx);
        self.rebuild_name_cache_from(name);
        Some(removed)
    }

    /// Rebuild name cache. Requires node names, provided externally.
    /// This is called with a mapping of node_id -> name.
    pub(crate) fn rebuild_name_cache_from(&mut self, _removed_name: &str) {
        // We can't rebuild fully here since we don't have access to the arena.
        // Instead, we remove the stale entry and shift indices.
        self.name_cache.remove(_removed_name);
        // Rebuild all indices from scratch — caller should use rebuild_name_cache_with_names
        // For now, just clear and note that the Fdt layer handles this correctly.
        self.name_cache.clear();
    }

    /// Rebuild name cache from a list of (name, index) pairs.
    pub(crate) fn rebuild_name_cache_with_names(&mut self, names: &[(String, usize)]) {
        self.name_cache.clear();
        for (name, idx) in names {
            self.name_cache.insert(name.clone(), *idx);
        }
    }

    /// Sets a property, adding it if it doesn't exist or updating if it does.
    pub fn set_property(&mut self, prop: Property) {
        let name = prop.name.clone();
        if let Some(&idx) = self.prop_cache.get(&name) {
            // Update existing property
            self.properties[idx] = prop;
        } else {
            // Add new property
            let idx = self.properties.len();
            self.prop_cache.insert(name, idx);
            self.properties.push(prop);
        }
    }

    /// Gets a property by name.
    pub fn get_property(&self, name: &str) -> Option<&Property> {
        self.prop_cache.get(name).map(|&idx| &self.properties[idx])
    }

    /// Gets a mutable reference to a property by name.
    pub fn get_property_mut(&mut self, name: &str) -> Option<&mut Property> {
        self.prop_cache
            .get(name)
            .map(|&idx| &mut self.properties[idx])
    }

    fn rebuild_prop_cache(&mut self) {
        self.prop_cache.clear();
        for (idx, prop) in self.properties.iter().enumerate() {
            self.prop_cache.insert(prop.name.clone(), idx);
        }
    }

    /// Removes a property by name.
    ///
    /// Updates indices after removal to keep the cache consistent.
    pub fn remove_property(&mut self, name: &str) -> Option<Property> {
        if let Some(&idx) = self.prop_cache.get(name) {
            let prop = self.properties.remove(idx);
            self.rebuild_prop_cache();
            Some(prop)
        } else {
            None
        }
    }

    /// Returns the `#address-cells` property value.
    pub fn address_cells(&self) -> Option<u32> {
        self.get_property("#address-cells")
            .and_then(|prop| prop.get_u32())
    }

    /// Returns the `#size-cells` property value.
    pub fn size_cells(&self) -> Option<u32> {
        self.get_property("#size-cells")
            .and_then(|prop| prop.get_u32())
    }

    /// Returns the `phandle` property value.
    pub fn phandle(&self) -> Option<Phandle> {
        self.get_property("phandle")
            .and_then(|prop| prop.get_u32())
            .map(Phandle::from)
    }

    /// Returns the local `interrupt-parent` property value.
    pub fn interrupt_parent(&self) -> Option<Phandle> {
        self.get_property("interrupt-parent")
            .and_then(|prop| prop.get_u32())
            .map(Phandle::from)
    }

    /// Returns the `status` property value.
    pub fn status(&self) -> Option<Status> {
        let prop = self.get_property("status")?;
        let s = prop.as_str()?;
        match s {
            "okay" => Some(Status::Okay),
            "disabled" => Some(Status::Disabled),
            _ => None,
        }
    }

    /// Parses the `ranges` property for address translation.
    ///
    /// Returns a vector of range entries mapping child bus addresses to parent bus addresses.
    pub fn ranges(&self, parent_address_cells: u32) -> Option<Vec<RangesEntry>> {
        let prop = self.get_property("ranges")?;
        let mut entries = Vec::new();
        let mut reader = prop.as_reader();

        let child_address_cells = self.address_cells().unwrap_or(2) as usize;
        let parent_addr_cells = parent_address_cells as usize;
        let size_cells = self.size_cells().unwrap_or(1) as usize;

        while let (Some(child_addr), Some(parent_addr), Some(size)) = (
            reader.read_cells(child_address_cells),
            reader.read_cells(parent_addr_cells),
            reader.read_cells(size_cells),
        ) {
            entries.push(RangesEntry {
                child_bus_address: child_addr,
                parent_bus_address: parent_addr,
                length: size,
            });
        }

        Some(entries)
    }

    /// Returns the `compatible` property as a string iterator.
    pub fn compatible(&self) -> Option<impl Iterator<Item = &str>> {
        let prop = self.get_property("compatible")?;
        Some(prop.as_str_iter())
    }

    /// Returns an iterator over all compatible strings.
    pub fn compatibles(&self) -> impl Iterator<Item = &str> {
        self.get_property("compatible")
            .map(|prop| prop.as_str_iter())
            .into_iter()
            .flatten()
    }

    /// Returns the `device_type` property value.
    pub fn device_type(&self) -> Option<&str> {
        let prop = self.get_property("device_type")?;
        prop.as_str()
    }

    /// Returns true if this node is a memory node.
    pub fn is_memory(&self) -> bool {
        if let Some(dt) = self.device_type()
            && dt == "memory"
        {
            return true;
        }
        self.name.starts_with("memory")
    }

    /// Returns true if this node is an interrupt controller.
    pub fn is_interrupt_controller(&self) -> bool {
        self.name.starts_with("interrupt-controller")
            || self.get_property("interrupt-controller").is_some()
    }

    /// Returns the `#interrupt-cells` property value.
    pub fn interrupt_cells(&self) -> Option<u32> {
        self.get_property("#interrupt-cells")
            .and_then(|prop| prop.get_u32())
    }

    /// Returns true if this node is a clock provider.
    pub fn is_clock(&self) -> bool {
        self.get_property("#clock-cells").is_some()
    }

    /// Returns true if this node is a PCI bridge.
    pub fn is_pci(&self) -> bool {
        self.device_type() == Some("pci")
    }
}

impl From<&fdt_raw::Node<'_>> for Node {
    fn from(raw: &fdt_raw::Node<'_>) -> Self {
        let mut new_node = Node::new(raw.name());
        // Copy properties only; children are managed by Fdt
        for raw_prop in raw.properties() {
            let prop = Property::from(&raw_prop);
            new_node.set_property(prop);
        }
        new_node
    }
}

impl Display for Node {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Node(name: {})", self.name)
    }
}

impl Debug for Node {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Node {{ name: {}, properties: {}, children: {} }}",
            self.name,
            self.properties.len(),
            self.children.len()
        )
    }
}
