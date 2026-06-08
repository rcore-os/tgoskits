//! Editable Flattened Device Tree (FDT) structure.
//!
//! This module provides the main `Fdt` type for creating, modifying, and
//! encoding device tree blobs. It supports loading from existing DTB files,
//! building new trees programmatically, and applying device tree overlays.
//!
//! All nodes are stored in a flat `BTreeMap<NodeId, Node>` arena. Child
//! relationships are represented as `Vec<NodeId>` inside each `Node`.

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

use crate::{
    FdtData, FdtEncoder, FdtError, Node, NodeId, NodeType, NodeTypeMut, NodeView, Phandle,
};

pub use fdt_raw::MemoryReservation;

/// An editable Flattened Device Tree (FDT).
///
/// All nodes are stored in a flat `BTreeMap<NodeId, Node>`. The tree structure
/// is maintained through `Vec<NodeId>` children lists in each `Node` and an
/// optional `parent: Option<NodeId>` back-pointer.
#[derive(Clone)]
pub struct Fdt {
    /// Boot CPU ID
    pub boot_cpuid_phys: u32,
    /// Memory reservation block entries
    pub memory_reservations: Vec<MemoryReservation>,
    /// Flat storage for all nodes
    nodes: BTreeMap<NodeId, Node>,
    /// Parent mapping: child_id -> parent_id
    parent_map: BTreeMap<NodeId, NodeId>,
    /// Root node ID
    root: NodeId,
    /// Next unique node ID to allocate
    next_id: NodeId,
    /// Cache mapping phandles to node IDs for fast lookup
    phandle_cache: BTreeMap<Phandle, NodeId>,
}

impl Default for Fdt {
    fn default() -> Self {
        Self::new()
    }
}

impl Fdt {
    /// Creates a new empty FDT with an empty root node.
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        let root_id: NodeId = 0;
        nodes.insert(root_id, Node::new(""));
        Self {
            boot_cpuid_phys: 0,
            memory_reservations: Vec::new(),
            nodes,
            parent_map: BTreeMap::new(),
            root: root_id,
            next_id: 1,
            phandle_cache: BTreeMap::new(),
        }
    }

    /// Allocates a new node in the arena, returning its unique ID.
    fn alloc_node(&mut self, node: Node) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.insert(id, node);
        id
    }

    /// Returns the root node ID.
    pub fn root_id(&self) -> NodeId {
        self.root
    }

    /// Returns the parent node ID for the given node, if any.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.parent_map.get(&id).copied()
    }

    /// Returns a reference to the node with the given ID.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Returns a mutable reference to the node with the given ID.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// Returns the total number of nodes in the tree.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Adds a new node as a child of `parent`, returning the new node's ID.
    ///
    /// Sets the new node's `parent` field and updates the parent's children list
    /// and name cache.
    pub fn add_node(&mut self, parent: NodeId, node: Node) -> NodeId {
        let name = node.name.clone();
        let id = self.alloc_node(node);
        self.parent_map.insert(id, parent);

        if let Some(parent_node) = self.nodes.get_mut(&parent) {
            parent_node.add_child(&name, id);
        }

        // Update phandle cache if the new node has a phandle
        if let Some(phandle) = self.nodes.get(&id).and_then(|n| n.phandle()) {
            self.phandle_cache.insert(phandle, id);
        }

        id
    }

    /// Removes a child node (by name) from the given parent, and recursively
    /// removes the entire subtree from the arena.
    ///
    /// Returns the removed node's ID if found.
    pub fn remove_node(&mut self, parent: NodeId, name: &str) -> Option<NodeId> {
        let removed_id = {
            let parent_node = self.nodes.get_mut(&parent)?;
            parent_node.remove_child(name)?
        };

        // Rebuild parent's name cache (needs arena access for child names)
        self.rebuild_name_cache(parent);

        // Recursively remove the subtree
        self.remove_subtree(removed_id);

        Some(removed_id)
    }

    /// Recursively removes a node and all its descendants from the arena.
    fn remove_subtree(&mut self, id: NodeId) {
        if let Some(node) = self.nodes.remove(&id) {
            // Remove from parent map
            self.parent_map.remove(&id);
            // Remove from phandle cache
            if let Some(phandle) = node.phandle() {
                self.phandle_cache.remove(&phandle);
            }
            // Recursively remove children
            for child_id in node.children() {
                self.remove_subtree(*child_id);
            }
        }
    }

    /// Rebuilds the name cache for a node based on its current children.
    fn rebuild_name_cache(&mut self, id: NodeId) {
        let names: Vec<(String, usize)> = {
            let node = match self.nodes.get(&id) {
                Some(n) => n,
                None => return,
            };
            node.children()
                .iter()
                .enumerate()
                .filter_map(|(idx, &child_id)| {
                    self.nodes.get(&child_id).map(|c| (c.name.clone(), idx))
                })
                .collect()
        };
        if let Some(node) = self.nodes.get_mut(&id) {
            node.rebuild_name_cache_with_names(&names);
        }
    }

    pub fn resolve_alias(&self, alias: &str) -> Option<&str> {
        let root = self.nodes.get(&self.root)?;
        let alias_node_id = root.get_child("aliases")?;
        let alias_node = self.nodes.get(&alias_node_id)?;
        let prop = alias_node.get_property(alias)?;
        prop.as_str()
    }

    /// 规范化路径：如果是别名则解析为完整路径，否则确保以 / 开头
    fn normalize_path(&self, path: &str) -> Option<String> {
        if path.starts_with('/') {
            Some(path.to_string())
        } else {
            // 尝试解析别名
            self.resolve_alias(path).map(|s| s.to_string())
        }
    }

    /// Looks up a node by its full path (e.g. "/soc/uart@10000"),
    /// returning its `NodeId`.
    ///
    /// The root node is matched by "/" or "".
    pub fn get_by_path_id(&self, path: &str) -> Option<NodeId> {
        let normalized_path = self.normalize_path(path)?;
        let normalized = normalized_path.trim_start_matches('/');
        if normalized.is_empty() {
            return Some(self.root);
        }

        let mut current = self.root;
        for part in normalized.split('/') {
            let node = self.nodes.get(&current)?;
            current = node.get_child(part)?;
        }
        Some(current)
    }

    /// Looks up a node by its phandle value, returning its `NodeId`.
    pub fn get_by_phandle_id(&self, phandle: Phandle) -> Option<NodeId> {
        self.phandle_cache.get(&phandle).copied()
    }

    /// Computes the full path string for a node by walking up parent links.
    pub fn path_of(&self, id: NodeId) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = id;
        while let Some(node) = self.nodes.get(&cur) {
            if cur == self.root {
                break;
            }
            parts.push(&node.name);
            match self.parent_map.get(&cur) {
                Some(&p) => cur = p,
                None => break,
            }
        }
        parts.reverse();
        if parts.is_empty() {
            return String::from("/");
        }
        format!("/{}", parts.join("/"))
    }

    /// Removes a node and its subtree by path.
    ///
    /// Returns the removed node's ID if found.
    pub fn remove_by_path(&mut self, path: &str) -> Option<NodeId> {
        let normalized = path.trim_start_matches('/');
        if normalized.is_empty() {
            return None; // Cannot remove root
        }

        let parts: Vec<&str> = normalized.split('/').collect();
        let child_name = *parts.last()?;

        // Find the parent node
        let parent_path = &parts[..parts.len() - 1];
        let mut parent_id = self.root;
        for &part in parent_path {
            let node = self.nodes.get(&parent_id)?;
            parent_id = node.get_child(part)?;
        }

        self.remove_node(parent_id, child_name)
    }

    /// Returns a depth-first iterator over all node IDs in the tree.
    pub fn iter_node_ids(&self) -> NodeDfsIter<'_> {
        NodeDfsIter {
            fdt: self,
            stack: vec![self.root],
        }
    }

    /// Parses an FDT from raw byte data.
    pub fn from_bytes(data: &[u8]) -> Result<Self, FdtError> {
        let raw_fdt = fdt_raw::Fdt::from_bytes(data)?;
        Self::from_raw(&raw_fdt)
    }

    /// Parses an FDT from a raw pointer.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the pointer is valid and points to a
    /// valid FDT data structure.
    pub unsafe fn from_ptr(ptr: *mut u8) -> Result<Self, FdtError> {
        let raw_fdt = unsafe { fdt_raw::Fdt::from_ptr(ptr)? };
        Self::from_raw(&raw_fdt)
    }

    /// Converts from a raw FDT parser instance.
    fn from_raw(raw_fdt: &fdt_raw::Fdt) -> Result<Self, FdtError> {
        let header = raw_fdt.header();

        let mut fdt = Fdt {
            boot_cpuid_phys: header.boot_cpuid_phys,
            memory_reservations: raw_fdt.memory_reservations().collect(),
            nodes: BTreeMap::new(),
            parent_map: BTreeMap::new(),
            root: 0,
            next_id: 0,
            phandle_cache: BTreeMap::new(),
        };

        // Build node tree using a stack to track parent node IDs.
        // raw_fdt.all_nodes() yields nodes in DFS pre-order with level info.
        // We use a stack of (NodeId, level) to find parents.
        let mut id_stack: Vec<(NodeId, usize)> = Vec::new();

        for raw_node in raw_fdt.all_nodes() {
            let level = raw_node.level();
            let node = Node::from(&raw_node);
            let node_name = node.name.clone();

            // Allocate the node in the arena
            let node_id = fdt.alloc_node(node);

            // Update phandle cache
            if let Some(phandle) = fdt.nodes.get(&node_id).and_then(|n| n.phandle()) {
                fdt.phandle_cache.insert(phandle, node_id);
            }

            // Pop the stack until we find the parent at level - 1
            while let Some(&(_, stack_level)) = id_stack.last() {
                if stack_level >= level {
                    id_stack.pop();
                } else {
                    break;
                }
            }

            if let Some(&(parent_id, _)) = id_stack.last() {
                // Set parent link
                fdt.parent_map.insert(node_id, parent_id);
                // Add as child to parent
                if let Some(parent) = fdt.nodes.get_mut(&parent_id) {
                    parent.add_child(&node_name, node_id);
                }
            } else {
                // This is the root node
                fdt.root = node_id;
            }

            id_stack.push((node_id, level));
        }

        Ok(fdt)
    }

    /// Looks up a node by path and returns an immutable classified view.
    pub fn get_by_path(&self, path: &str) -> Option<NodeType<'_>> {
        let id = self.get_by_path_id(path)?;
        Some(NodeView::new(self, id).classify())
    }

    /// Looks up a node by path and returns a mutable classified view.
    pub fn get_by_path_mut(&mut self, path: &str) -> Option<NodeTypeMut<'_>> {
        let id = self.get_by_path_id(path)?;
        Some(NodeView::new(self, id).classify_mut())
    }

    /// Looks up a node by phandle and returns an immutable classified view.
    pub fn get_by_phandle(&self, phandle: crate::Phandle) -> Option<NodeType<'_>> {
        let id = self.get_by_phandle_id(phandle)?;
        Some(NodeView::new(self, id).classify())
    }

    /// Looks up a node by phandle and returns a mutable classified view.
    pub fn get_by_phandle_mut(&mut self, phandle: crate::Phandle) -> Option<NodeTypeMut<'_>> {
        let id = self.get_by_phandle_id(phandle)?;
        Some(NodeView::new(self, id).classify_mut())
    }

    /// Returns a depth-first iterator over `NodeView`s.
    fn iter_raw_nodes(&self) -> impl Iterator<Item = NodeView<'_>> {
        self.iter_node_ids().map(move |id| NodeView::new(self, id))
    }

    /// Returns a depth-first iterator over classified `NodeType`s.
    pub fn all_nodes(&self) -> impl Iterator<Item = NodeType<'_>> {
        self.iter_raw_nodes().map(|v| v.classify())
    }

    pub fn root_mut(&mut self) -> NodeTypeMut<'_> {
        self.view_typed_mut(self.root).unwrap()
    }

    /// Finds nodes with matching compatible strings.
    pub fn find_compatible(&self, compatible: &[&str]) -> Vec<NodeType<'_>> {
        let mut results = Vec::new();
        for node_ref in self.all_nodes() {
            let compatibles = node_ref.as_node().compatibles();
            let mut found = false;

            for comp in compatibles {
                if compatible.contains(&comp) {
                    results.push(node_ref);
                    found = true;
                    break;
                }
            }

            if found {
                continue;
            }
        }
        results
    }

    /// Encodes the FDT to DTB binary format.
    pub fn encode(&self) -> FdtData {
        FdtEncoder::new(self).encode()
    }
}

/// Depth-first iterator over all node IDs in the tree.
pub struct NodeDfsIter<'a> {
    fdt: &'a Fdt,
    stack: Vec<NodeId>,
}

impl<'a> Iterator for NodeDfsIter<'a> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.stack.pop()?;
        if let Some(node) = self.fdt.nodes.get(&id) {
            // Push children in reverse order so that the first child is visited first
            for &child_id in node.children().iter().rev() {
                self.stack.push(child_id);
            }
        }
        Some(id)
    }
}
