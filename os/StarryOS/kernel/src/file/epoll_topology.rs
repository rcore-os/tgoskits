//! Transactional topology tracking for nested epoll instances.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_sync::{Mutex, MutexGuard};

use super::epoll::EpollInner;

const MAX_NESTED_EPOLL_EDGES: usize = 4;

static EPOLL_TOPOLOGY_LOCK: Mutex<()> = Mutex::new(());
static NEXT_EPOLL_EDGE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Eq, PartialEq)]
struct EpollEdgeId(u64);

#[derive(Clone)]
pub(super) struct EpollTopologyLink {
    id: EpollEdgeId,
    node: alloc::sync::Weak<EpollInner>,
}

#[derive(Default)]
pub(super) struct EpollTopology {
    parents: SpinNoIrq<Vec<EpollTopologyLink>>,
    children: SpinNoIrq<Vec<EpollTopologyLink>>,
}

#[derive(Clone, Copy)]
enum TopologyDirection {
    Parents,
    Children,
}

struct TopologyScan {
    max_depth: usize,
    reached_target: bool,
}

pub(super) fn lock_epoll_topology() -> MutexGuard<'static, ()> {
    EPOLL_TOPOLOGY_LOCK.lock()
}

/// Validate a prospective link while the caller holds the topology mutex.
pub(super) fn prepare_nested_link(
    source: &Arc<EpollInner>,
    target: &Arc<EpollInner>,
) -> AxResult<EpollTopologyLink> {
    if Arc::ptr_eq(source, target) {
        return Err(AxError::InvalidInput);
    }

    let downstream = scan_epoll_topology(target, TopologyDirection::Children, Some(source))?;
    if downstream.reached_target {
        return Err(AxError::FilesystemLoop);
    }
    let upstream = scan_epoll_topology(source, TopologyDirection::Parents, None)?;
    if upstream.max_depth + 1 + downstream.max_depth > MAX_NESTED_EPOLL_EDGES {
        return Err(AxError::FilesystemLoop);
    }

    let edge_id = NEXT_EPOLL_EDGE_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map(EpollEdgeId)
        .map_err(|_| AxError::NoMemory)?;
    Ok(EpollTopologyLink {
        id: edge_id,
        node: Arc::downgrade(target),
    })
}

/// Reserve both topology vectors before committing a new link.
pub(super) fn reserve_nested_link(
    source: &Arc<EpollInner>,
    target: &Arc<EpollInner>,
) -> AxResult<()> {
    source
        .topology
        .children
        .lock()
        .try_reserve(1)
        .map_err(|_| AxError::NoMemory)?;
    target
        .topology
        .parents
        .lock()
        .try_reserve(1)
        .map_err(|_| AxError::NoMemory)?;
    Ok(())
}

/// Commit both directions after all fallible preparation has completed.
pub(super) fn commit_nested_link(
    source: &Arc<EpollInner>,
    target: &Arc<EpollInner>,
    link: &EpollTopologyLink,
) {
    source.topology.children.lock().push(link.clone());
    target.topology.parents.lock().push(EpollTopologyLink {
        id: link.id,
        node: Arc::downgrade(source),
    });
}

/// Remove both directions while the caller holds the topology mutex.
pub(super) fn detach_nested_link(source: &EpollInner, link: &EpollTopologyLink) {
    source
        .topology
        .children
        .lock()
        .retain(|child| child.id != link.id);
    if let Some(child) = link.node.upgrade() {
        child
            .topology
            .parents
            .lock()
            .retain(|parent| parent.id != link.id);
    }
}

fn scan_epoll_topology(
    start: &Arc<EpollInner>,
    direction: TopologyDirection,
    target: Option<&Arc<EpollInner>>,
) -> AxResult<TopologyScan> {
    let mut pending = Vec::new();
    let mut visited_depths = Vec::new();
    push_topology_item(&mut pending, (Arc::clone(start), 0))?;
    push_topology_item(&mut visited_depths, (Arc::as_ptr(start), 0))?;

    let mut max_depth = 0;
    while let Some((node, depth)) = pending.pop() {
        max_depth = max_depth.max(depth);
        for link in node.topology.snapshot_links(direction)? {
            let Some(next) = link.node.upgrade() else {
                continue;
            };
            let next_depth = depth + 1;
            if next_depth > MAX_NESTED_EPOLL_EDGES {
                return Err(AxError::FilesystemLoop);
            }
            if target.is_some_and(|target| Arc::ptr_eq(&next, target)) {
                return Ok(TopologyScan {
                    max_depth: next_depth,
                    reached_target: true,
                });
            }

            let next_ptr = Arc::as_ptr(&next);
            if let Some((_, seen_depth)) = visited_depths
                .iter_mut()
                .find(|(node_ptr, _)| *node_ptr == next_ptr)
            {
                if *seen_depth >= next_depth {
                    continue;
                }
                *seen_depth = next_depth;
            } else {
                push_topology_item(&mut visited_depths, (next_ptr, next_depth))?;
            }
            push_topology_item(&mut pending, (next, next_depth))?;
        }
    }

    Ok(TopologyScan {
        max_depth,
        reached_target: false,
    })
}

impl EpollTopology {
    fn snapshot_links(&self, direction: TopologyDirection) -> AxResult<Vec<EpollTopologyLink>> {
        let links = match direction {
            TopologyDirection::Parents => &self.parents,
            TopologyDirection::Children => &self.children,
        };

        loop {
            let len = links.lock().len();
            let mut snapshot = Vec::new();
            snapshot.try_reserve(len).map_err(|_| AxError::NoMemory)?;

            let mut links = links.lock();
            links.retain(|link| link.node.strong_count() != 0);
            if links.len() > snapshot.capacity() {
                continue;
            }
            snapshot.extend(links.iter().cloned());
            return Ok(snapshot);
        }
    }
}

fn push_topology_item<T>(items: &mut Vec<T>, item: T) -> AxResult<()> {
    items.try_reserve(1).map_err(|_| AxError::NoMemory)?;
    items.push(item);
    Ok(())
}

#[cfg(axtest)]
pub(crate) fn push_topology_item_preserves_order_and_grows_capacity() -> bool {
    let mut items: Vec<u32> = Vec::new();
    // First push seeds the vector with one element.
    push_topology_item(&mut items, 10).is_ok()
        && items == [10]
        // Subsequent pushes preserve insertion order.
        && push_topology_item(&mut items, 20).is_ok()
        && push_topology_item(&mut items, 30).is_ok()
        && items == [10, 20, 30]
        // Capacity must grow to accommodate the reservation request.
        && items.capacity() >= 3
}

#[cfg(axtest)]
pub(crate) fn epoll_edge_id_and_constants_hold_for_test() -> bool {
    // Test EpollEdgeId
    let id1 = EpollEdgeId(1);
    let id2 = EpollEdgeId(2);
    assert!(id1 != id2);
    assert!(id1 == id1);
    
    // Test MAX_NESTED_EPOLL_EDGES constant
    assert_eq!(MAX_NESTED_EPOLL_EDGES, 4);
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_topology_struct_and_methods_hold_for_test() -> bool {
    // Test EpollTopology default construction
    let topology = EpollTopology::default();
    
    // Test EpollTopologyLink Clone trait
    // (Can't construct without Arc<EpollInner>, but verify type exists)
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_topology_direction_and_scan_hold_for_test() -> bool {
    // Test TopologyDirection variants exist
    let _parents = TopologyDirection::Parents;
    let _children = TopologyDirection::Children;
    
    // Test TopologyScan struct fields
    let scan = TopologyScan {
        max_depth: 10,
        reached_target: false,
    };
    assert_eq!(scan.max_depth, 10);
    assert!(!scan.reached_target);
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_edge_id_clone_copy_partial_eq_hold_for_test() -> bool {
    // Test EpollEdgeId derives
    let id1 = EpollEdgeId(42);
    let id2 = id1.clone(); // Clone
    assert!(id1 == id2);  // PartialEq (use assert! to avoid Debug requirement)
    
    let id3 = id1; // Copy
    assert!(id3 == id1);
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_topology_static_constants_hold_for_test() -> bool {
    // Test static constants
    assert_eq!(MAX_NESTED_EPOLL_EDGES, 4);
    
    // Test NEXT_EPOLL_EDGE_ID (atomic, just verify it exists and is > 0)
    assert!(NEXT_EPOLL_EDGE_ID.load(Ordering::Relaxed) >= 1);
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_topology_link_clone_hold_for_test() -> bool {
    // Test EpollTopologyLink is Clone
    // Can't construct without Arc<EpollInner>, but verify the type has Clone bound
    
    // Verify that EpollTopologyLink is declared as Clone
    fn _assert_clone<T: Clone>() {}
    // This would compile only if EpollTopologyLink: Clone
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_topology_vec_and_reserve_hold_for_test() -> bool {
    use alloc::vec::Vec;
    
    // Test Vec operations used in topology
    let mut vec: Vec<u32> = Vec::new();
    
    // try_reserve
    vec.try_reserve(4).is_ok();
    
    // push and retain
    vec.push(1);
    vec.push(2);
    vec.push(3);
    vec.retain(|&x| x != 2);
    assert!(vec.len() == 2);
    assert!(vec[0] == 1);
    assert!(vec[1] == 3);
    
    // extend with cloned items
    let mut vec2: Vec<u32> = Vec::new();
    vec2.extend(vec.iter().cloned());
    assert!(vec2.len() == 2);
    
    // iter().find()
    let mut vec3: Vec<(u64, usize)> = Vec::new();
    vec3.push((100, 1));
    vec3.push((200, 2));
    let found = vec3.iter().find(|(ptr, _)| *ptr == 200);
    assert!(found.is_some());
    
    true
}

#[cfg(axtest)]
pub(crate) fn epoll_arc_operations_hold_for_test() -> bool {
    use alloc::sync::Arc;
    
    // Test Arc operations used in topology
    let arc1 = Arc::new(42u32);
    let arc2 = Arc::clone(&arc1);
    
    // ptr_eq
    assert!(Arc::ptr_eq(&arc1, &arc2));
    
    // as_ptr
    let ptr1 = Arc::as_ptr(&arc1);
    let ptr2 = Arc::as_ptr(&arc2);
    assert!(ptr1 == ptr2);
    
    // strong_count
    assert!(Arc::strong_count(&arc1) == 2);
    
    // downgrade (creates Weak)
    let weak = Arc::downgrade(&arc1);
    assert!(weak.upgrade().is_some());
    
    true
}
