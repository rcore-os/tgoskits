// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.

use crate::types::{RbNode, RbRoot, RbRootCached};

/// Initializer for an empty red-black tree root.
pub const RB_ROOT: RbRoot = RbRoot {
    node: core::ptr::null_mut(),
};

/// Check if a red-black tree root is empty.
#[inline]
pub fn rb_empty_root(root: &RbRoot) -> bool {
    root.node.is_null()
}

/// Conversion helper trait for intrusive tree node pointers.
pub trait RbNodePointer {
    /// Return the raw pointer to the underlying `RbNode`.
    fn into_rb_node_ptr(self) -> *mut RbNode;
}

impl RbNodePointer for *mut RbNode {
    #[inline]
    fn into_rb_node_ptr(self) -> *mut RbNode {
        self
    }
}

impl RbNodePointer for &mut RbNode {
    #[inline]
    fn into_rb_node_ptr(self) -> *mut RbNode {
        self
    }
}

impl RbNodePointer for &RbNode {
    #[inline]
    fn into_rb_node_ptr(self) -> *mut RbNode {
        self as *const RbNode as *mut RbNode
    }
}

/// Conversion helper trait for node pointers passed to erasure routines.
pub trait RbNodePtr {
    /// Return the raw pointer to the underlying `RbNode`.
    fn rb_node_ptr(self) -> *mut RbNode;
}

impl RbNodePtr for *mut RbNode {
    #[inline]
    fn rb_node_ptr(self) -> *mut RbNode {
        self
    }
}

impl RbNodePtr for &mut RbNode {
    #[inline]
    fn rb_node_ptr(self) -> *mut RbNode {
        self
    }
}

impl RbNodePtr for &RbNode {
    #[inline]
    fn rb_node_ptr(self) -> *mut RbNode {
        (self as *const RbNode).cast_mut()
    }
}

/// Check if an intrusive node is considered unlinked/empty.
///
/// # Safety
/// `node` must convert to a non-null, aligned pointer to an initialized `RbNode`
/// whose `parent_color` field can be safely read. Concurrent modifications must
/// be synchronized.
pub unsafe fn rb_empty_node<N: RbNodePointer>(node: N) -> bool {
    let ptr = node.into_rb_node_ptr();
    assert!(!ptr.is_null());
    // SAFETY: `ptr` is checked non-null by the assertion above. The caller guarantees
    // it points to a properly aligned, initialized `RbNode` valid for reading its
    // `parent_color` without conflicting data races.
    unsafe { (*ptr).parent_color == ptr as usize }
}

/// Mark an intrusive node as unlinked/empty.
///
/// # Safety
/// `node` must convert to a non-null, aligned pointer to an initialized `RbNode`
/// with exclusive mutable access. Overwriting `parent_color` must not race with
/// any reader or writer.
pub unsafe fn rb_clear_node<N: RbNodePointer>(node: N) {
    let ptr = node.into_rb_node_ptr();
    assert!(!ptr.is_null());
    // SAFETY: `ptr` is verified non-null by the assertion above. The caller guarantees
    // exclusive mutable access to the aligned `RbNode` allocation, so writing its
    // own pointer address into `parent_color` is sound and race-free.
    unsafe { (*ptr).parent_color = ptr as usize };
}

const COLOR_RED: usize = 0;
const COLOR_BLACK: usize = 1;
const COLOR_MASK: usize = 1;
const PARENT_MASK: usize = !3;

#[inline]
const fn unpack_parent(parent_color: usize) -> *mut RbNode {
    (parent_color & PARENT_MASK) as *mut RbNode
}

#[inline]
const fn unpack_color(parent_color: usize) -> usize {
    parent_color & COLOR_MASK
}

/// Extract the parent pointer encoded in `node.parent_color`.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` whose
/// `parent_color` is readable without data races. Null pointers return null.
#[inline]
unsafe fn node_parent(node: *mut RbNode) -> *mut RbNode {
    if node.is_null() {
        core::ptr::null_mut()
    } else {
        // SAFETY: `node` is verified non-null above and points to an initialized,
        // aligned `RbNode`. Dereferencing reads the bit-packed `parent_color`
        // without concurrent conflicting writes.
        unsafe { unpack_parent((*node).parent_color) }
    }
}

/// Extract the color bit encoded in `node.parent_color`.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` whose
/// `parent_color` is readable. Null pointers are treated as black nodes.
#[inline]
unsafe fn node_color(node: *mut RbNode) -> usize {
    if node.is_null() {
        COLOR_BLACK
    } else {
        // SAFETY: `node` is non-null, properly aligned, and points to an initialized
        // `RbNode`. Dereferencing to read the lowest color bit in `parent_color`
        // is valid and race-free.
        unsafe { unpack_color((*node).parent_color) }
    }
}

/// Check whether `node` is colored red.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` whose
/// `parent_color` is readable. Null nodes are treated as black.
#[inline]
unsafe fn is_red_node(node: *mut RbNode) -> bool {
    if node.is_null() {
        false
    } else {
        // SAFETY: `node` is non-null and points to an aligned, initialized `RbNode`;
        // invoking `node_color` on it satisfies `node_color`'s safety contract.
        unsafe { node_color(node) == COLOR_RED }
    }
}

/// Check whether `node` is colored black (null nodes are black by definition).
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` whose
/// `parent_color` is readable. Null nodes return true.
#[inline]
unsafe fn is_black_node(node: *mut RbNode) -> bool {
    if node.is_null() {
        true
    } else {
        // SAFETY: `node` is non-null and points to an aligned, initialized `RbNode`;
        // delegating to `node_color` satisfies its precondition.
        unsafe { node_color(node) == COLOR_BLACK }
    }
}

/// Update the parent pointer in `node.parent_color` while preserving the color bit.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` with exclusive
/// write access. `parent` must be an aligned pointer to an `RbNode` or null.
#[inline]
unsafe fn set_parent_ptr(node: *mut RbNode, parent: *mut RbNode) {
    if !node.is_null() {
        // SAFETY: `node` is confirmed non-null. The caller holds exclusive mutable access
        // to `*node`, so reading `parent_color` to preserve the color bit and writing
        // the new parent address into the upper bits is sound and does not race.
        unsafe {
            let color = unpack_color((*node).parent_color);
            (*node).parent_color = (parent as usize & PARENT_MASK) | color;
        }
    }
}

/// Update the color bit in `node.parent_color` while preserving the parent pointer.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` with exclusive
/// write access.
#[inline]
unsafe fn set_color_val(node: *mut RbNode, color: usize) {
    if !node.is_null() {
        // SAFETY: `node` is confirmed non-null. The caller holds exclusive mutable access
        // to `*node`, allowing reading and updating the lowest color bit of `parent_color`
        // without altering the parent pointer or racing with concurrent accesses.
        unsafe {
            let parent_bits = (*node).parent_color & PARENT_MASK;
            (*node).parent_color = parent_bits | (color & COLOR_MASK);
        }
    }
}

/// Augmentation hooks for red-black trees carrying subtree summaries.
#[repr(C)]
pub struct RbAugment {
    /// Recomputes the subtree summary from the first node up towards the second node.
    pub propagate: unsafe fn(*mut RbNode, *mut RbNode),
    /// Copies summary metadata from the old node to the new node.
    pub copy: unsafe fn(*mut RbNode, *mut RbNode),
    /// Called when `old` is rotated down and replaced by `new` as the subtree root.
    pub rotate: unsafe fn(*mut RbNode, *mut RbNode),
}

/// Perform a left tree rotation around `node`, updating links and triggering augmentation.
///
/// # Safety
/// - `root` must point to an initialized, live pointer slot representing the tree root.
/// - `node` must be a valid, aligned node in the tree whose `right` child is non-null.
/// - Caller must hold exclusive mutable ownership of the tree.
/// - If `aug` is non-null, its function pointers must be valid and callable.
unsafe fn rotate_left(root: *mut *mut RbNode, node: *mut RbNode, aug: *const RbAugment) {
    // SAFETY: `node` is in an exclusively owned tree and has a non-null `right` child (`pivot`).
    // Re-linking `pivot.left` to `node.right`, reparenting via `set_parent_ptr`, updating `*root`
    // (or parent's child link), and attaching `node` as `pivot.left` mutates exclusively owned,
    // valid node fields within the tree allocation. If `aug` is non-null, invoking
    // `(*aug).rotate(node, pivot)` executes the caller's augmentation hook with valid nodes.
    unsafe {
        let pivot = (*node).right;
        debug_assert!(!pivot.is_null());

        let b = (*pivot).left;
        (*node).right = b;
        if !b.is_null() {
            set_parent_ptr(b, node);
        }

        let p = node_parent(node);
        set_parent_ptr(pivot, p);
        if p.is_null() {
            *root = pivot;
        } else if (*p).left == node {
            (*p).left = pivot;
        } else {
            (*p).right = pivot;
        }

        (*pivot).left = node;
        set_parent_ptr(node, pivot);

        if !aug.is_null() {
            ((*aug).rotate)(node, pivot);
        }
    }
}

/// Perform a right tree rotation around `node`, updating links and triggering augmentation.
///
/// # Safety
/// - `root` must point to an initialized, live pointer slot representing the tree root.
/// - `node` must be a valid, aligned node in the tree whose `left` child is non-null.
/// - Caller must hold exclusive mutable ownership of the tree.
/// - If `aug` is non-null, its function pointers must be valid and callable.
unsafe fn rotate_right(root: *mut *mut RbNode, node: *mut RbNode, aug: *const RbAugment) {
    // SAFETY: `node` is part of an exclusively owned tree with a non-null `left` child (`pivot`).
    // Re-linking `pivot.right` to `node.left`, reparenting via `set_parent_ptr`, updating `*root`
    // (or parent's child pointer), and attaching `node` as `pivot.right` mutates only valid,
    // exclusively owned node fields. If `aug` is non-null, `(*aug).rotate(node, pivot)`
    // safely invokes the caller-provided hook with live nodes.
    unsafe {
        let pivot = (*node).left;
        debug_assert!(!pivot.is_null());

        let b = (*pivot).right;
        (*node).left = b;
        if !b.is_null() {
            set_parent_ptr(b, node);
        }

        let p = node_parent(node);
        set_parent_ptr(pivot, p);
        if p.is_null() {
            *root = pivot;
        } else if (*p).left == node {
            (*p).left = pivot;
        } else {
            (*p).right = pivot;
        }

        (*pivot).right = node;
        set_parent_ptr(node, pivot);

        if !aug.is_null() {
            ((*aug).rotate)(node, pivot);
        }
    }
}

/// Link an unlinked node into a tree as a child of `parent` at the given `link` slot.
///
/// # Safety
/// - `node` must be an aligned, non-null pointer to unlinked, writable memory for an `RbNode`.
/// - `parent` must be null (if inserting as root) or an aligned, valid `RbNode` pointer in the tree.
/// - `link` must point to the specific pointer slot (such as `&mut root.node` or `&mut (*parent).left` / `&mut (*parent).right`) where the child pointer is stored.
/// - Caller must maintain exclusive access to the tree and follow up with `rb_insert_color` or `insert_color`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rb_link_node(
    node: *mut RbNode,
    parent: *mut RbNode,
    link: *mut *mut RbNode,
) {
    // SAFETY: `node` is guaranteed by caller to be non-null, aligned, and pointing to
    // writable `RbNode` memory. Writing `parent as usize` initializes `parent_color`
    // (parent address with RED color 0), and setting `left` and `right` to null leaves
    // the node with clean leaf links.
    unsafe {
        (*node).parent_color = parent as usize;
        (*node).left = core::ptr::null_mut();
        (*node).right = core::ptr::null_mut();
    }
    // SAFETY: `link` points to a live, exclusively accessible pointer slot in the caller's
    // tree structure. Writing `node` stores the newly linked child pointer without aliasing.
    unsafe {
        *link = node;
    }
}

/// Rebalance the red-black tree after inserting a node.
///
/// # Safety
/// - If `root` or `node` is null, this function returns immediately without side effects.
/// - If non-null, `root` must point to an initialized `RbRoot`.
/// - If non-null, `node` must be an initialized `RbNode` linked into `root` as a RED node.
/// - The caller must hold exclusive access to the entire tree structure for the duration of the call.
/// - If `aug` is non-null, its callbacks must be valid and conform to `RbAugment`.
pub unsafe fn insert_color(mut node: *mut RbNode, root: *mut RbRoot, aug: *const RbAugment) {
    if root.is_null() || node.is_null() {
        return;
    }

    // SAFETY: `root` is verified non-null above and points to an initialized `RbRoot`.
    // Deriving a raw pointer to `(*root).node` via `addr_of_mut!` avoids creating
    // an intermediate reference while obtaining the root pointer slot.
    let root_ptr = unsafe { core::ptr::addr_of_mut!((*root).node) };

    // SAFETY: `node` is linked into the caller-owned tree rooted at `*root_ptr`.
    // The loop ascends parent and grandparent pointers (which are valid, aligned nodes
    // in the tree), inspects/modifies colors via `node_parent`, `is_red_node`, and
    // `set_color_val`, and performs rotations on exclusively owned subtrees.
    // Finally, recoloring `*root_ptr` to black operates on the valid tree root.
    unsafe {
        while let Some(parent) = {
            let p = node_parent(node);
            if !p.is_null() && is_red_node(p) {
                Some(p)
            } else {
                None
            }
        } {
            let gparent = node_parent(parent);
            if gparent.is_null() {
                break;
            }

            if (*gparent).left == parent {
                let uncle = (*gparent).right;
                if is_red_node(uncle) {
                    set_color_val(parent, COLOR_BLACK);
                    set_color_val(uncle, COLOR_BLACK);
                    set_color_val(gparent, COLOR_RED);
                    node = gparent;
                } else {
                    if (*parent).right == node {
                        node = parent;
                        rotate_left(root_ptr, node, aug);
                    }
                    let current_parent = node_parent(node);
                    let current_gparent = node_parent(current_parent);
                    set_color_val(current_parent, COLOR_BLACK);
                    set_color_val(current_gparent, COLOR_RED);
                    rotate_right(root_ptr, current_gparent, aug);
                    break;
                }
            } else {
                let uncle = (*gparent).left;
                if is_red_node(uncle) {
                    set_color_val(parent, COLOR_BLACK);
                    set_color_val(uncle, COLOR_BLACK);
                    set_color_val(gparent, COLOR_RED);
                    node = gparent;
                } else {
                    if (*parent).left == node {
                        node = parent;
                        rotate_right(root_ptr, node, aug);
                    }
                    let current_parent = node_parent(node);
                    let current_gparent = node_parent(current_parent);
                    set_color_val(current_parent, COLOR_BLACK);
                    set_color_val(current_gparent, COLOR_RED);
                    rotate_left(root_ptr, current_gparent, aug);
                    break;
                }
            }
        }

        if !(*root_ptr).is_null() {
            set_color_val(*root_ptr, COLOR_BLACK);
        }
    }
}

/// Restore red-black tree invariants after linking a node.
///
/// # Safety
/// If non-null, `node` must be an `RbNode` linked into `root` with color RED.
/// If non-null, `root` must point to an initialized `RbRoot`.
/// Caller must hold exclusive access to the tree. (Null `node` or `root` is a safe no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rb_insert_color(node: *mut RbNode, root: *mut RbRoot) {
    // SAFETY: `insert_color` is called with a null augmentation pointer, performing
    // standard red-black insertion fixup on `node` within `root` under the caller's
    // exclusive tree ownership.
    unsafe {
        insert_color(node, root, core::ptr::null());
    }
}

/// Rebalance after inserting `node`, propagating augmentation updates via `aug`.
///
/// # Safety
/// If non-null, `node` must be linked into `root` as a RED node.
/// If non-null, `root` must point to an initialized `RbRoot`.
/// If `aug` is non-null, its callbacks must be valid and uphold augmentation invariants.
/// Caller must hold exclusive access to the tree. (Null `node` or `root` is a safe no-op).
pub unsafe fn rb_insert_augmented(node: *mut RbNode, root: *mut RbRoot, aug: *const RbAugment) {
    // SAFETY: Forwarding `node`, `root`, and `aug` to `insert_color` satisfies its preconditions
    // because `node` is linked into `root` and the caller guarantees exclusive tree ownership.
    unsafe {
        insert_color(node, root, aug);
    }
}

/// Rebalance a cached tree after linking `node`, updating `leftmost` if requested.
///
/// # Safety
/// If non-null, `root` must point to an initialized `RbRootCached` under exclusive caller access.
/// If non-null, `node` must be an `RbNode` linked into `root.root` as a RED node.
/// `leftmost` must be true if and only if `node` is the new minimum key in the tree.
/// (Null `root` or `node` returns immediately without effect).
pub unsafe fn rb_insert_color_cached(node: *mut RbNode, root: *mut RbRootCached, leftmost: bool) {
    if root.is_null() || node.is_null() {
        return;
    }
    // SAFETY: `root` and `node` are non-null. Exclusive ownership of `*root` allows safely
    // updating `(*root).leftmost` without races, and `addr_of_mut!((*root).root)` derives
    // a valid pointer to the embedded `RbRoot` for rebalancing by `insert_color`.
    unsafe {
        if leftmost {
            (*root).leftmost = node;
        }
        insert_color(
            node,
            core::ptr::addr_of_mut!((*root).root),
            core::ptr::null(),
        );
    }
}

/// Rebalance an augmented cached tree after linking `node`, updating `leftmost` if requested.
///
/// # Safety
/// If non-null, `root` must point to an initialized `RbRootCached` under exclusive caller access.
/// If non-null, `node` must be linked into `root.root` as a RED node.
/// `leftmost` must be true if and only if `node` is the new minimum key in the tree.
/// If `aug` is non-null, its callbacks must be valid. (Null `root` or `node` is a no-op).
pub unsafe fn rb_insert_augmented_cached(
    node: *mut RbNode,
    root: *mut RbRootCached,
    leftmost: bool,
    aug: *const RbAugment,
) {
    if root.is_null() || node.is_null() {
        return;
    }
    // SAFETY: `root` and `node` are verified non-null. The caller's exclusive access to `*root`
    // ensures updating `(*root).leftmost` and rebalancing `root.root` with `aug` operates
    // on valid, unaliased memory without data races.
    unsafe {
        if leftmost {
            (*root).leftmost = node;
        }
        insert_color(node, core::ptr::addr_of_mut!((*root).root), aug);
    }
}

/// Rebalance the red-black tree after a black node has been spliced out.
///
/// # Safety
/// - `root` must point to an initialized, live pointer slot containing the tree root.
/// - `parent` must be null (if rebalancing reached root) or an aligned, valid node in the tree.
/// - `child` represents the node deficient in black height (may be null if a black leaf was deleted).
/// - Caller must hold exclusive mutable ownership of the tree.
/// - If `aug` is non-null, its callbacks must be valid and callable.
unsafe fn erase_rebalance(
    root: *mut *mut RbNode,
    mut child: *mut RbNode,
    mut parent: *mut RbNode,
    aug: *const RbAugment,
) {
    // SAFETY: The loop operates on the exclusively owned tree rooted at `*root`.
    // `parent` and `sibling` pointers refer to valid tree nodes; reading and writing
    // color bits through `node_color`, `is_red_node`, and `set_color_val` mutates
    // only aligned `parent_color` fields. Rotations through `rotate_left` and `rotate_right`
    // update parent/child links and trigger `aug` callbacks on live nodes.
    // Setting `child`'s color to black on exit affects either a valid node or is skipped if null.
    unsafe {
        while child != *root && is_black_node(child) {
            if parent.is_null() {
                break;
            }

            if (*parent).left == child {
                let mut sibling = (*parent).right;
                if is_red_node(sibling) {
                    set_color_val(sibling, COLOR_BLACK);
                    set_color_val(parent, COLOR_RED);
                    rotate_left(root, parent, aug);
                    sibling = (*parent).right;
                }

                if sibling.is_null()
                    || (is_black_node((*sibling).left) && is_black_node((*sibling).right))
                {
                    if !sibling.is_null() {
                        set_color_val(sibling, COLOR_RED);
                    }
                    child = parent;
                    parent = node_parent(child);
                } else {
                    if is_black_node((*sibling).right) {
                        set_color_val((*sibling).left, COLOR_BLACK);
                        set_color_val(sibling, COLOR_RED);
                        rotate_right(root, sibling, aug);
                        sibling = (*parent).right;
                    }
                    set_color_val(sibling, node_color(parent));
                    set_color_val(parent, COLOR_BLACK);
                    set_color_val((*sibling).right, COLOR_BLACK);
                    rotate_left(root, parent, aug);
                    child = *root;
                    break;
                }
            } else {
                let mut sibling = (*parent).left;
                if is_red_node(sibling) {
                    set_color_val(sibling, COLOR_BLACK);
                    set_color_val(parent, COLOR_RED);
                    rotate_right(root, parent, aug);
                    sibling = (*parent).left;
                }

                if sibling.is_null()
                    || (is_black_node((*sibling).left) && is_black_node((*sibling).right))
                {
                    if !sibling.is_null() {
                        set_color_val(sibling, COLOR_RED);
                    }
                    child = parent;
                    parent = node_parent(child);
                } else {
                    if is_black_node((*sibling).left) {
                        set_color_val((*sibling).right, COLOR_BLACK);
                        set_color_val(sibling, COLOR_RED);
                        rotate_left(root, sibling, aug);
                        sibling = (*parent).left;
                    }
                    set_color_val(sibling, node_color(parent));
                    set_color_val(parent, COLOR_BLACK);
                    set_color_val((*sibling).left, COLOR_BLACK);
                    rotate_right(root, parent, aug);
                    child = *root;
                    break;
                }
            }
        }

        if !child.is_null() {
            set_color_val(child, COLOR_BLACK);
        }
    }
}

/// Unlink `node` from the tree rooted at `*root`, rebalance, and update augmentation.
///
/// # Safety
/// - If `node` or `root` is null, returns immediately with no side effects.
/// - If non-null, `node` must be an active member of the tree rooted at `*root`.
/// - `root` must point to an initialized, live pointer slot for the tree root.
/// - Caller must hold exclusive mutable access to the tree and all reachable nodes.
/// - If `aug` is non-null, its callbacks must be valid and conform to `RbAugment`.
unsafe fn erase_node(node: *mut RbNode, root: *mut *mut RbNode, aug: *const RbAugment) {
    if node.is_null() || root.is_null() {
        return;
    }

    // SAFETY: `node` and `root` are non-null. `node` is spliced out directly if it has at most
    // one child, or replaced by its in-order successor `succ` (located by traversing left spine).
    // Updating `*root`, parent child links, and child parent pointers via `set_parent_ptr`
    // mutates only valid, exclusively owned nodes. If the spliced node had black color,
    // `erase_rebalance` restores black height. `(*aug).propagate` recomputes summaries along
    // affected paths (`fixup_parent` and `spliced`). Finally, clearing `node`'s pointers
    // and `parent_color` leaves it safely detached from the tree.
    unsafe {
        let mut spliced: *mut RbNode = node;
        let spliced_color: usize;
        let replacement: *mut RbNode;
        let fixup_parent: *mut RbNode;

        if (*node).left.is_null() {
            replacement = (*node).right;
            fixup_parent = node_parent(node);
            spliced_color = node_color(node);

            let p = fixup_parent;
            if p.is_null() {
                *root = replacement;
            } else if (*p).left == node {
                (*p).left = replacement;
            } else {
                (*p).right = replacement;
            }
            if !replacement.is_null() {
                set_parent_ptr(replacement, p);
            }
        } else if (*node).right.is_null() {
            replacement = (*node).left;
            fixup_parent = node_parent(node);
            spliced_color = node_color(node);

            let p = fixup_parent;
            if p.is_null() {
                *root = replacement;
            } else if (*p).left == node {
                (*p).left = replacement;
            } else {
                (*p).right = replacement;
            }
            if !replacement.is_null() {
                set_parent_ptr(replacement, p);
            }
        } else {
            let mut succ = (*node).right;
            while !(*succ).left.is_null() {
                succ = (*succ).left;
            }
            spliced = succ;
            spliced_color = node_color(succ);
            replacement = (*succ).right;

            let succ_parent = node_parent(succ);
            if succ_parent == node {
                fixup_parent = succ;
                if !replacement.is_null() {
                    set_parent_ptr(replacement, succ);
                }
            } else {
                fixup_parent = succ_parent;
                if (*succ_parent).left == succ {
                    (*succ_parent).left = replacement;
                } else {
                    (*succ_parent).right = replacement;
                }
                if !replacement.is_null() {
                    set_parent_ptr(replacement, succ_parent);
                }
                (*succ).right = (*node).right;
                set_parent_ptr((*node).right, succ);
            }

            let node_p = node_parent(node);
            if node_p.is_null() {
                *root = succ;
            } else if (*node_p).left == node {
                (*node_p).left = succ;
            } else {
                (*node_p).right = succ;
            }
            set_parent_ptr(succ, node_p);

            (*succ).left = (*node).left;
            set_parent_ptr((*node).left, succ);

            set_color_val(succ, node_color(node));
        }

        if spliced_color == COLOR_BLACK {
            erase_rebalance(root, replacement, fixup_parent, aug);
        }

        if !aug.is_null() {
            if !fixup_parent.is_null() {
                ((*aug).propagate)(fixup_parent, core::ptr::null_mut());
            }
            if spliced != node && !spliced.is_null() {
                ((*aug).propagate)(spliced, core::ptr::null_mut());
            }
        }

        (*node).left = core::ptr::null_mut();
        (*node).right = core::ptr::null_mut();
        (*node).parent_color = 0;
    }
}

/// Remove an intrusive node from a red-black tree root.
///
/// # Safety
/// `node` must convert to a non-null, aligned pointer to an `RbNode` that is currently
/// an active member of the tree rooted at `root`. The caller must maintain exclusive
/// access to `root` and ensure all tree nodes remain live.
pub unsafe fn rb_erase_impl<N: RbNodePtr>(node: N, root: &mut RbRoot) {
    let ptr = node.rb_node_ptr();
    assert!(!ptr.is_null());
    // SAFETY: `ptr` is verified non-null by the assertion above and belongs to `root`.
    // `root` is an exclusive mutable reference, so `addr_of_mut!(root.node)` provides
    // a valid, unaliased pointer slot to pass to `erase_node` with a null augment hook.
    unsafe {
        erase_node(ptr, core::ptr::addr_of_mut!(root.node), core::ptr::null());
    }
}

/// Erase `node` from a cached tree, maintaining the cached leftmost pointer.
pub fn rb_erase_cached<N: RbNodePtr>(node: N, root: &mut RbRootCached) {
    let ptr = node.rb_node_ptr();
    assert!(!ptr.is_null());
    // SAFETY: `ptr` is checked non-null by assertion. The caller's exclusive `&mut RbRootCached`
    // borrow ensures no concurrent access. Under the intrusive container contract that `ptr`
    // belongs to `root.root`, if `ptr` equals `root.leftmost`, its in-order successor `rb_next(ptr)`
    // correctly replaces it as the new leftmost. `addr_of_mut!(root.root.node)` yields the
    // root pointer slot for `erase_node` to safely remove `ptr`.
    unsafe {
        if core::ptr::eq(root.leftmost, ptr) {
            root.leftmost = rb_next(ptr);
        }
        erase_node(
            ptr,
            core::ptr::addr_of_mut!(root.root.node),
            core::ptr::null(),
        );
    }
}

/// Erase `node` from `root` and rebalance the tree.
///
/// # Safety
/// `root` must be non-null and point to an aligned, initialized `RbRoot`.
/// `node` must be a non-null, aligned pointer to an `RbNode` currently linked in `root`.
/// Caller must ensure exclusive access to the tree during erasure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rb_erase(node: *mut RbNode, root: *mut RbRoot) {
    // SAFETY: `root` is a valid, aligned pointer to `RbRoot`. Dereferencing as `&mut *root`
    // gives exclusive access for the call duration, and `node` is a valid member of `root`,
    // fulfilling `rb_erase_impl`'s safety contract.
    unsafe {
        rb_erase_impl(node, &mut *root);
    }
}

/// Erase `node` from `root`, rebalance, and update augmented summaries.
///
/// # Safety
/// If non-null, `node` must be an active member of `root`.
/// If non-null, `root` must point to an initialized `RbRoot`.
/// If `aug` is non-null, its callbacks must be valid and conform to `RbAugment`.
/// Caller must hold exclusive access to the tree. (Null `node` or `root` is a no-op).
pub unsafe fn rb_erase_augmented(node: *mut RbNode, root: *mut RbRoot, aug: *const RbAugment) {
    if node.is_null() || root.is_null() {
        return;
    }
    // SAFETY: `node` and `root` are checked non-null above. `addr_of_mut!((*root).node)`
    // derives the root pointer slot, and `erase_node` removes `node` from the exclusively
    // owned tree using `aug` callbacks without aliasing conflicts.
    unsafe {
        erase_node(node, core::ptr::addr_of_mut!((*root).node), aug);
    }
}

/// Erase `node` from an augmented cached tree, updating summaries and cached leftmost.
///
/// # Safety
/// If non-null, `node` must be an active member of `root.root`.
/// If non-null, `root` must point to an initialized `RbRootCached` under exclusive caller access.
/// If `aug` is non-null, its callbacks must be valid. (Null `node` or `root` is a no-op).
pub unsafe fn rb_erase_augmented_cached(
    node: *mut RbNode,
    root: *mut RbRootCached,
    aug: *const RbAugment,
) {
    if root.is_null() || node.is_null() {
        return;
    }
    // SAFETY: `node` and `root` are non-null. Exclusive ownership of `*root` ensures updating
    // `(*root).leftmost` and taking a pointer to `root.root.node` for `erase_node` operates
    // on valid, exclusively accessible tree structures without data races.
    unsafe {
        if core::ptr::eq((*root).leftmost, node) {
            (*root).leftmost = rb_next(node);
        }
        erase_node(node, core::ptr::addr_of_mut!((*root).root.node), aug);
    }
}

/// Return the parent pointer of `node`, or null if `node` is root or null.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` whose `parent_color`
/// is readable without concurrent data races. (Null `node` returns null).
pub unsafe fn rb_parent(node: *mut RbNode) -> *mut RbNode {
    if node.is_null() {
        core::ptr::null_mut()
    } else {
        // SAFETY: `node` is verified non-null in this branch. Passing it to `node_parent`
        // satisfies its safety contract as `node` is an aligned, initialized `RbNode`.
        unsafe { node_parent(node) }
    }
}

/// Return the first (leftmost) node in the red-black tree, or null if the tree is empty.
///
/// # Safety
/// If non-null, `root` must point to an initialized `RbRoot` whose tree nodes are aligned,
/// initialized, and not concurrently mutated without synchronization. (Null `root` returns null).
#[inline]
pub unsafe fn rb_first(root: *const RbRoot) -> *mut RbNode {
    if root.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `root` is verified non-null above. Reading `(*root).node` accesses the initialized
    // root pointer slot under the caller's shared read guarantee.
    let mut curr = unsafe { (*root).node };
    while !curr.is_null() {
        // SAFETY: `curr` is non-null and is an initialized node reached along the left spine
        // of the live tree; reading its `left` child pointer is valid and race-free.
        let left = unsafe { (*curr).left };
        if left.is_null() {
            break;
        }
        curr = left;
    }
    curr
}

/// Return the last (rightmost) node in the red-black tree, or null if the tree is empty.
///
/// # Safety
/// If non-null, `root` must point to an initialized `RbRoot` whose tree nodes are aligned,
/// initialized, and not concurrently mutated without synchronization. (Null `root` returns null).
pub unsafe fn rb_last(root: *const RbRoot) -> *mut RbNode {
    if root.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `root` is verified non-null above. Dereferencing reads the initialized `node`
    // field of `RbRoot` under the caller's shared read guarantee.
    let mut curr = unsafe { (*root).node };
    while !curr.is_null() {
        // SAFETY: `curr` is non-null and is an initialized node reached along the right spine
        // of the live tree; reading its `right` child pointer is valid and race-free.
        let right = unsafe { (*curr).right };
        if right.is_null() {
            break;
        }
        curr = right;
    }
    curr
}

/// Return the in-order successor of `node`, or null if none exists.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` within a valid tree
/// whose links are consistent and free of concurrent conflicting writes. (Null `node` returns null).
pub unsafe fn rb_next(node: *mut RbNode) -> *mut RbNode {
    if node.is_null() {
        return core::ptr::null_mut();
    }

    // SAFETY: `node` is non-null. Dereferencing `(*node).right` reads an initialized child pointer.
    // If a right subtree exists, traversing its left spine reads valid `(*curr).left` pointers.
    // Otherwise, ascending through `node_parent(curr)` and reading `(*p).right` examines live
    // parent nodes in the caller's tree without concurrent mutation.
    unsafe {
        let right = (*node).right;
        if !right.is_null() {
            let mut curr = right;
            while !(*curr).left.is_null() {
                curr = (*curr).left;
            }
            return curr;
        }

        let mut curr = node;
        let mut p = node_parent(curr);
        while !p.is_null() && (*p).right == curr {
            curr = p;
            p = node_parent(curr);
        }
        p
    }
}

/// Return the in-order predecessor of `node`, or null if none exists.
///
/// # Safety
/// If non-null, `node` must point to an aligned, initialized `RbNode` within a valid tree
/// whose links are consistent and free of concurrent conflicting writes. (Null `node` returns null).
pub unsafe fn rb_prev(node: *mut RbNode) -> *mut RbNode {
    if node.is_null() {
        return core::ptr::null_mut();
    }

    // SAFETY: `node` is non-null. Dereferencing `(*node).left` reads an initialized child pointer.
    // If a left subtree exists, traversing its right spine reads valid `(*curr).right` pointers.
    // Otherwise, ascending through `node_parent(curr)` and reading `(*p).left` examines live
    // parent nodes in the caller's tree without concurrent mutation.
    unsafe {
        let left = (*node).left;
        if !left.is_null() {
            let mut curr = left;
            while !(*curr).right.is_null() {
                curr = (*curr).right;
            }
            return curr;
        }

        let mut curr = node;
        let mut p = node_parent(curr);
        while !p.is_null() && (*p).left == curr {
            curr = p;
            p = node_parent(curr);
        }
        p
    }
}

/// Return the cached leftmost node, or null if the tree is empty.
#[inline]
pub fn rb_first_cached(root: &RbRootCached) -> *mut RbNode {
    root.leftmost
}

/// Replace `victim` with `new` in `root` without rebalancing.
///
/// # Safety
/// - If any argument is null, this function returns immediately without modifying state.
/// - If non-null, `root` must point to an initialized `RbRoot`.
/// - `victim` must be a valid, active node in the tree rooted at `root`.
/// - `new` must point to an aligned, initialized, unlinked `RbNode`.
/// - Caller must hold exclusive mutable access to `root`, `victim`, `new`, and all connected nodes.
pub unsafe fn rb_replace_node(victim: *mut RbNode, new: *mut RbNode, root: *mut RbRoot) {
    if victim.is_null() || new.is_null() || root.is_null() {
        return;
    }

    // SAFETY: `victim`, `new`, and `root` are non-null. `parent` is obtained via `node_parent`.
    // Redirecting `parent`'s child pointer or `*root_node` points the tree link to `new`.
    // Reparenting `victim`'s children to `new` via `set_parent_ptr` and copying `parent_color`,
    // `left`, and `right` to `new` installs it in place. Clearing `victim`'s pointers and
    // `parent_color` detaches it from the exclusively owned tree without aliasing issues.
    unsafe {
        let parent = node_parent(victim);
        let root_node = core::ptr::addr_of_mut!((*root).node);

        if parent.is_null() {
            *root_node = new;
        } else if (*parent).left == victim {
            (*parent).left = new;
        } else {
            (*parent).right = new;
        }

        let left = (*victim).left;
        if !left.is_null() {
            set_parent_ptr(left, new);
        }
        let right = (*victim).right;
        if !right.is_null() {
            set_parent_ptr(right, new);
        }

        (*new).parent_color = (*victim).parent_color;
        (*new).left = left;
        (*new).right = right;

        (*victim).left = core::ptr::null_mut();
        (*victim).right = core::ptr::null_mut();
        (*victim).parent_color = 0;
    }
}

/// Replace `victim` with `new` in `root` without clearing `victim`'s pointers.
///
/// # Safety
/// - If any argument is null, this function returns immediately without modifying state.
/// - If non-null, `root` must point to an initialized `RbRoot`.
/// - `victim` must be a valid, active node in the tree rooted at `root`.
/// - `new` must point to an aligned, initialized, unlinked `RbNode`.
/// - Caller must hold exclusive writer access to the tree; concurrent readers must be within
///   RCU read-side critical sections.
/// - `victim` must remain valid and unmodified until all concurrent RCU readers finish their grace period.
pub unsafe fn rb_replace_node_rcu(victim: *mut RbNode, new: *mut RbNode, root: *mut RbRoot) {
    if victim.is_null() || new.is_null() || root.is_null() {
        return;
    }

    // SAFETY: `victim`, `new`, and `root` are non-null. `new` is initialized with `victim`'s
    // `parent_color`, `left`, and `right`, and child parent pointers are updated to `new`
    // before publishing `new` to `parent` (or `*root_node`). `victim`'s pointers are deliberately
    // preserved so concurrent RCU readers traversing through `victim` can still proceed safely.
    unsafe {
        let parent = node_parent(victim);
        let root_node = core::ptr::addr_of_mut!((*root).node);

        let left = (*victim).left;
        let right = (*victim).right;

        (*new).parent_color = (*victim).parent_color;
        (*new).left = left;
        (*new).right = right;

        if !left.is_null() {
            set_parent_ptr(left, new);
        }
        if !right.is_null() {
            set_parent_ptr(right, new);
        }

        if parent.is_null() {
            *root_node = new;
        } else if (*parent).left == victim {
            (*parent).left = new;
        } else {
            (*parent).right = new;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct IntNode {
        node: RbNode,
        val: i32,
    }

    fn new_int_node(val: i32) -> IntNode {
        IntNode {
            node: RbNode {
                parent_color: 0,
                left: core::ptr::null_mut(),
                right: core::ptr::null_mut(),
            },
            val,
        }
    }

    /// Link and balance an `IntNode` into the test tree.
    ///
    /// # Safety
    /// `root` must point to an initialized `RbRoot`. `item` must point to an initialized,
    /// unlinked `IntNode` allocation that remains live for the test tree's lifetime.
    /// Caller must hold exclusive mutable access to `*root`.
    unsafe fn insert_val(root: *mut RbRoot, item: *mut IntNode) {
        // SAFETY: `item` is guaranteed non-null, aligned, and valid by caller; deriving a pointer
        // to its embedded `node` field produces a valid `*mut RbNode`.
        let node_ptr = unsafe { core::ptr::addr_of_mut!((*item).node) };
        let mut parent: *mut RbNode = core::ptr::null_mut();
        // SAFETY: `root` is non-null and points to an initialized `RbRoot` under exclusive access.
        let mut link: *mut *mut RbNode = unsafe { core::ptr::addr_of_mut!((*root).node) };

        // SAFETY: Traversal follows valid, initialized `*link` pointers down the tree until finding
        // an empty slot. `rb_link_node` links `node_ptr` into the slot, and `rb_insert_color`
        // rebalances the exclusively owned test tree.
        unsafe {
            while !(*link).is_null() {
                parent = *link;
                let cur = parent.cast::<IntNode>();
                if (*item).val < (*cur).val {
                    link = core::ptr::addr_of_mut!((*parent).left);
                } else {
                    link = core::ptr::addr_of_mut!((*parent).right);
                }
            }
            rb_link_node(node_ptr, parent, link);
            rb_insert_color(node_ptr, root);
        }
    }

    /// Recursively verify red-black tree invariants (black height, red node children, parent pointers).
    ///
    /// # Safety
    /// If non-null, `node` must point to an initialized `RbNode` within a valid tree hierarchy
    /// that is not concurrently mutated.
    unsafe fn verify_rb_invariants(node: *mut RbNode) -> usize {
        if node.is_null() {
            return 1;
        }
        // SAFETY: `node` is non-null and points to an initialized node in the test tree.
        // Reading `left` and `right` child pointers and parent pointers inspects live memory
        // without mutation. Recursing on child nodes traverses valid descendants.
        unsafe {
            if is_red_node(node) {
                assert!(is_black_node((*node).left));
                assert!(is_black_node((*node).right));
            }
            if !(*node).left.is_null() {
                assert_eq!(node_parent((*node).left), node);
            }
            if !(*node).right.is_null() {
                assert_eq!(node_parent((*node).right), node);
            }
            let left_bh = verify_rb_invariants((*node).left);
            let right_bh = verify_rb_invariants((*node).right);
            assert_eq!(left_bh, right_bh);
            left_bh + usize::from(is_black_node(node))
        }
    }

    #[test]
    fn c_abi_insert_balances_and_preserves_inorder_sequence() {
        let mut entries = [
            new_int_node(30),
            new_int_node(10),
            new_int_node(40),
            new_int_node(5),
            new_int_node(20),
            new_int_node(50),
            new_int_node(15),
        ];
        let mut root = RB_ROOT;
        assert!(rb_empty_root(&root));

        for entry in &mut entries {
            // SAFETY: `entry` is a live, stack-allocated `IntNode` with a unique address.
            // `&mut root` provides exclusive access to an initialized tree root on the stack.
            unsafe {
                insert_val(&mut root, entry);
            }
        }

        assert!(!rb_empty_root(&root));
        // SAFETY: `root.node` is the root of the stack-allocated test tree. Inspecting its
        // color and recursively checking tree invariants reads live, initialized memory.
        unsafe {
            assert!(is_black_node(root.node));
            verify_rb_invariants(root.node);
        }

        // Ascending traversal
        let mut sorted = [0i32; 7];
        // SAFETY: `root` is a valid, immutable `RbRoot` on the stack containing 7 nodes.
        // `rb_first` and `rb_next` traverse the in-order sequence of live stack nodes.
        unsafe {
            let mut curr = rb_first(&root);
            for slot in &mut sorted {
                assert!(!curr.is_null());
                *slot = (*curr.cast::<IntNode>()).val;
                curr = rb_next(curr);
            }
            assert!(curr.is_null());
        }
        assert_eq!(sorted, [5, 10, 15, 20, 30, 40, 50]);

        // Descending traversal
        let mut rev_sorted = [0i32; 7];
        // SAFETY: `root` is a valid `RbRoot` on the stack. `rb_last` and `rb_prev` traverse
        // the live stack nodes in reverse in-order sequence without modifying them.
        unsafe {
            let mut curr = rb_last(&root);
            for slot in &mut rev_sorted {
                assert!(!curr.is_null());
                *slot = (*curr.cast::<IntNode>()).val;
                curr = rb_prev(curr);
            }
            assert!(curr.is_null());
        }
        assert_eq!(rev_sorted, [50, 40, 30, 20, 15, 10, 5]);
    }

    #[test]
    fn erase_maintains_balance_and_order() {
        let values = [25, 10, 45, 5, 20, 35, 60, 1, 8, 15, 22, 30, 40, 50, 70];
        let mut nodes: [IntNode; 15] = core::array::from_fn(|i| new_int_node(values[i]));
        let mut root = RB_ROOT;

        for node in &mut nodes {
            // SAFETY: `node` is a live, stack-allocated `IntNode` with a unique memory location.
            // Passing `&mut root` maintains exclusive access to the tree root.
            unsafe {
                insert_val(&mut root, node);
            }
        }

        // SAFETY: `root.node` points to the root of the newly constructed stack tree.
        // Validating invariants reads live, initialized memory without mutation.
        unsafe {
            verify_rb_invariants(root.node);
        }

        // Remove elements in arbitrary order: leaf, single child, two children, root
        let erase_indices = [0, 4, 1, 8, 14, 6, 2, 3, 5, 7, 9, 10, 11, 12, 13];
        let mut remaining_count = nodes.len();

        for &idx in &erase_indices {
            let target = core::ptr::addr_of_mut!(nodes[idx].node);
            // SAFETY: `target` is a valid pointer to a stack-allocated node confirmed to be an active
            // member of `root`. `&mut root` grants exclusive access for `rb_erase` to unlink it.
            // Subsequent invariant checks inspect the updated, live tree.
            unsafe {
                rb_erase(target, &mut root);
                remaining_count -= 1;
                if remaining_count > 0 {
                    assert!(is_black_node(root.node));
                    verify_rb_invariants(root.node);
                } else {
                    assert!(rb_empty_root(&root));
                }
            }
        }
    }

    #[test]
    fn cached_first_tracks_erase_of_leftmost_node() {
        let mut first = new_int_node(10);
        let mut second = new_int_node(20);

        let mut root = RbRootCached {
            root: RB_ROOT,
            leftmost: core::ptr::null_mut(),
        };

        // SAFETY: `first.node` is an unlinked, stack-allocated `RbNode`. Linking it at `root.root.node`
        // and calling `rb_insert_color_cached` with `leftmost = true` initializes the tree and caches `first`.
        unsafe {
            rb_link_node(&mut first.node, core::ptr::null_mut(), &mut root.root.node);
            rb_insert_color_cached(&mut first.node, &mut root, true);
        }
        assert!(core::ptr::eq(rb_first_cached(&root), &mut first.node));

        // SAFETY: `second.node` is stack-allocated with a higher key (20 > 10). Linking it as the
        // right child of `first.node` and calling `rb_insert_color_cached` with `leftmost = false`
        // preserves `first` as the cached leftmost pointer.
        unsafe {
            rb_link_node(&mut second.node, &mut first.node, &mut first.node.right);
            rb_insert_color_cached(&mut second.node, &mut root, false);
        }
        assert!(core::ptr::eq(rb_first_cached(&root), &mut first.node));

        rb_erase_cached(&mut first.node, &mut root);
        assert!(core::ptr::eq(root.leftmost, &mut second.node));
        assert!(core::ptr::eq(root.root.node, &mut second.node));

        rb_erase_cached(&mut second.node, &mut root);
        assert!(root.leftmost.is_null());
        assert!(rb_empty_root(&root.root));
    }

    #[test]
    fn node_clear_and_empty_flags() {
        let mut node = new_int_node(42).node;
        // SAFETY: `node` is a live stack-allocated `RbNode`. `rb_clear_node` writes `&node as usize`
        // into `parent_color`, and `rb_empty_node` reads it back to verify the empty marker.
        unsafe {
            rb_clear_node(&mut node);
            assert!(rb_empty_node(&node));
            assert!(rb_empty_node(&mut node));
            assert!(rb_empty_node(&mut node as *mut RbNode));
        }
    }

    #[test]
    fn replace_node_preserves_tree_structure() {
        let mut root = RB_ROOT;
        let mut n1 = new_int_node(10);
        let mut n2 = new_int_node(20);
        let mut n3 = new_int_node(30);
        let mut replacement = new_int_node(20);

        // SAFETY: `n1`, `n2`, `n3`, and `replacement` are live stack-allocated `IntNode` instances.
        // `insert_val` builds an initial valid tree containing `n1`, `n2`, and `n3`.
        // `rb_replace_node` swaps `n2.node` with `replacement.node` in `root` without rebalancing.
        // Subsequent traversal via `rb_first` and `rb_next` reads the updated live sequence.
        unsafe {
            insert_val(&mut root, &mut n1);
            insert_val(&mut root, &mut n2);
            insert_val(&mut root, &mut n3);
            verify_rb_invariants(root.node);

            rb_replace_node(&mut n2.node, &mut replacement.node, &mut root);
            assert!(is_black_node(root.node));
            verify_rb_invariants(root.node);

            let mut vals = [0i32; 3];
            let mut curr = rb_first(&root);
            for v in &mut vals {
                assert!(!curr.is_null());
                *v = (*curr.cast::<IntNode>()).val;
                curr = rb_next(curr);
            }
            assert_eq!(vals, [10, 20, 30]);
        }
    }
}
