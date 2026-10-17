// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux 7.2.3 list primitives used by the upstream DRM translations. The
// node layout is supplied by the embedding record; links remain intrusive.

use core::{
    mem::MaybeUninit,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::types::{ListHead, LlistHead};

/// Initialize Linux's lock-free list head.
#[inline]
/// # Safety
/// `head` must be non-null, properly aligned to `LlistHead`, and point to
/// valid, initialized memory. The caller must have exclusive access to `head`
/// during initialization.
pub unsafe fn init_llist_head(head: *mut LlistHead) {
    // SAFETY: `head` is non-null and aligned; `addr_of_mut!` accesses `(*head).first`
    // without creating intermediate references, and `from_ptr` creates an `AtomicPtr`
    // to perform a relaxed store of null.
    unsafe { AtomicPtr::from_ptr(core::ptr::addr_of_mut!((*head).first)) }
        .store(core::ptr::null_mut(), Ordering::Relaxed);
}

/// Push one node onto a Linux llist. Returns true when the list was empty.
#[inline]
/// # Safety
/// `head` must be non-null, properly aligned, and point to a live `LlistHead`.
/// `node` must be non-null, properly aligned, unlinked, and exclusively owned
/// until atomic publication. Concurrent callers may safely call `llist_add` on `head`.
pub unsafe fn llist_add(node: *mut crate::types::LlistNode, head: *mut LlistHead) -> bool {
    // SAFETY: `head` is non-null and aligned; `addr_of_mut!` avoids intermediate references
    // to obtain an `AtomicPtr` for concurrent compare-and-swap operations.
    let first = unsafe { AtomicPtr::from_ptr(core::ptr::addr_of_mut!((*head).first)) };
    let mut old = first.load(Ordering::Relaxed);
    loop {
        // SAFETY: node is an exclusive, aligned pointer to unlinked storage; write initial next pointer before atomic publish.
        unsafe { (*node).next = old };
        match first.compare_exchange_weak(old, node, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return old.is_null(),
            Err(actual) => old = actual,
        }
    }
}

/// Push a linked batch onto a Linux llist. Returns true when the list was empty.
#[inline]
/// # Safety
/// `head` must be non-null, properly aligned, and point to a live `LlistHead`.
/// `first_node` and `last_node` must be non-null, aligned pointers representing
/// the head and tail of an unlinked linear batch of `LlistNode`s, exclusively
/// owned by the caller.
pub unsafe fn llist_add_batch(
    first_node: *mut crate::types::LlistNode,
    last_node: *mut crate::types::LlistNode,
    head: *mut LlistHead,
) -> bool {
    // SAFETY: head is non-null, aligned, and points to a valid LlistHead; addr_of_mut! avoids intermediate references.
    let first = unsafe { AtomicPtr::from_ptr(core::ptr::addr_of_mut!((*head).first)) };
    let mut old = first.load(Ordering::Relaxed);
    loop {
        // SAFETY: last_node is an exclusive, aligned pointer to the tail of the batch; write its next link before atomic CAS.
        unsafe { (*last_node).next = old };
        match first.compare_exchange_weak(old, first_node, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return old.is_null(),
            Err(actual) => old = actual,
        }
    }
}

/// Atomically detach every node from an llist.
#[inline]
/// # Safety
/// `head` must be non-null, properly aligned, and point to a live `LlistHead`.
/// The detached chain returned to the caller becomes exclusively owned.
pub unsafe fn llist_del_all(head: *mut LlistHead) -> *mut crate::types::LlistNode {
    // SAFETY: `head` is non-null and aligned; `addr_of_mut!` borrows `(*head).first`
    // as a raw pointer for atomic swap without intermediate reference creation.
    unsafe { AtomicPtr::from_ptr(core::ptr::addr_of_mut!((*head).first)) }
        .swap(core::ptr::null_mut(), Ordering::Acquire)
}

#[allow(non_snake_case)]
/// # Safety
/// `head` must be non-null, properly aligned to `ListHead`, and point to valid
/// writable memory. The caller must have exclusive access to `head`.
pub unsafe fn INIT_LIST_HEAD(head: *mut ListHead) {
    // SAFETY: `head` is non-null, aligned, and exclusively accessed; initializes
    // `next` and `prev` to point to `head` itself, forming an empty circular list.
    unsafe {
        (*head).next = head;
        (*head).prev = head;
    }
}

pub fn list_empty(head: &ListHead) -> bool {
    core::ptr::eq(head.next, head as *const ListHead as *mut ListHead)
}

pub fn list_is_singular(head: &ListHead) -> bool {
    !list_empty(head) && core::ptr::eq(head.next, head.prev)
}

/// # Safety
/// `new`, `prev`, and `next` must be non-null, aligned, and point to live `ListHead`
/// structs. `prev` and `next` must be consecutive nodes in a circular list. The caller
/// must hold exclusive access to all three nodes to prevent data races.
unsafe fn __list_add(new: *mut ListHead, prev: *mut ListHead, next: *mut ListHead) {
    // SAFETY: `prev`, `next`, and `new` are valid, aligned pointers with exclusive access;
    // links `new` between `prev` and `next` by updating adjacent pointers.
    unsafe {
        (*next).prev = new;
        (*new).next = next;
        (*new).prev = prev;
        (*prev).next = new;
    }
}

/// # Safety
/// `head` must point to an initialized circular list head, and `new` must point to
/// an unlinked, aligned `ListHead`. The caller must hold exclusive access to `head`,
/// `new`, and `head.next`.
pub unsafe fn list_add(new: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `head` and `new` are valid and exclusively accessed; `(*head).next` is
    // the current first element, splicing `new` immediately after `head`.
    unsafe {
        __list_add(new, head, (*head).next);
    }
}

/// C ABI provider for the LinuxKPI `list_sort()` contract.
///
/// The generic Linux implementation is GPL-only and is not copied here. This
/// independent, allocation-free stable insertion sort has the same ordering
/// contract; the engine-registration list is small and sorted once during
/// device setup.
#[unsafe(no_mangle)]
/// # Safety
/// `head` must point to an initialized circular doubly-linked list. All nodes
/// linked in `head` must remain valid, aligned, and exclusively accessed for
/// the duration of the sort. `cmp` must be a valid comparison function obeying
/// strict weak ordering.
pub unsafe extern "C" fn list_sort(
    priv_: *mut core::ffi::c_void,
    head: *mut ListHead,
    cmp: unsafe extern "C" fn(*mut core::ffi::c_void, *const ListHead, *const ListHead) -> i32,
) {
    // SAFETY: head is an aligned, non-null pointer to a circular list; detach all elements into a temporary list.
    let mut node = unsafe { (*head).next };
    // SAFETY: resetting head to an empty circular list while elements are detached and sorted.
    unsafe {
        (*head).next = head;
        (*head).prev = head;
    }

    while !core::ptr::eq(node, head) {
        // SAFETY: node is an initialized ListHead entry detached from the input list.
        let next = unsafe { (*node).next };
        // SAFETY: head is the valid head of the sorted accumulator list.
        let mut cursor = unsafe { (*head).next };
        // SAFETY: cursor and node are valid ListHead pointers within the list; cmp function pointer satisfies C ABI.
        while !core::ptr::eq(cursor, head) && unsafe { cmp(priv_, cursor, node) <= 0 } {
            // SAFETY: cursor is a valid node within the circular sorted list.
            cursor = unsafe { (*cursor).next };
        }

        // SAFETY: splicing node before cursor in the circular list; all adjacent link pointers are valid.
        unsafe {
            (*node).prev = (*cursor).prev;
            (*node).next = cursor;
            (*(*cursor).prev).next = node;
            (*cursor).prev = node;
        }
        node = next;
    }
}

/// # Safety
/// `head` must point to an initialized circular list head, and `new` must point
/// to an unlinked, aligned `ListHead`. Exclusive access to `head`, `new`, and
/// `head.prev` is required.
pub unsafe fn list_add_tail(new: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `head` is a valid circular list and `new` is unlinked; `(*head).prev`
    // is the current tail, splicing `new` between tail and `head`.
    unsafe {
        __list_add(new, (*head).prev, head);
    }
}

/// # Safety
/// `prev` and `next` must be non-null, aligned pointers to live `ListHead` nodes
/// in a circular list, with exclusive access to update `prev.next` and `next.prev`.
unsafe fn __list_del(prev: *mut ListHead, next: *mut ListHead) {
    // SAFETY: `prev` and `next` are valid, aligned pointers with exclusive access;
    // connects `prev` and `next` directly to remove the intermediate entry.
    unsafe {
        (*next).prev = prev;
        (*prev).next = next;
    }
}

#[inline]
/// # Safety
/// `entry` must be a non-null, aligned pointer to an initialized `ListHead` currently
/// linked in a circular list. The caller must have exclusive access to `entry`,
/// `entry.prev`, and `entry.next`.
pub unsafe fn __list_del_entry(entry: *mut ListHead) {
    // SAFETY: `entry` is a valid linked node; dereferences `prev` and `next` pointers
    // to unlink `entry` via `__list_del`.
    unsafe { __list_del((*entry).prev, (*entry).next) };
}

/// # Safety
/// `entry` must be a non-null, aligned pointer to an initialized `ListHead` currently
/// linked in a circular list, with exclusive access to `entry` and its neighbors.
pub unsafe fn list_del(entry: *mut ListHead) {
    // SAFETY: `entry` is linked and exclusively accessed; unlinks from neighbors via
    // `__list_del` and writes Linux debug poison values to `next` and `prev`.
    unsafe {
        __list_del((*entry).prev, (*entry).next);
        (*entry).next = 0xdead_0000_0000_0100usize as *mut ListHead;
        (*entry).prev = 0xdead_0000_0000_0122usize as *mut ListHead;
    }
}

/// # Safety
/// `entry` must be a non-null, aligned pointer to an initialized `ListHead` currently
/// linked in a circular list, with exclusive access to `entry` and its neighbors.
pub unsafe fn list_del_init(entry: *mut ListHead) {
    // SAFETY: `entry` is linked and exclusively accessed; unlinks from neighbors via
    // `__list_del` and reinitializes `entry` as a self-referencing empty list.
    unsafe {
        __list_del((*entry).prev, (*entry).next);
        INIT_LIST_HEAD(entry);
    }
}

/// # Safety
/// `entry` must be currently linked in a circular list, and `head` must be an initialized
/// circular list head. The caller must have exclusive access to `entry`, its current
/// neighbors, and `head`.
pub unsafe fn list_move(entry: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `entry` and `head` are valid and exclusively accessed; unlinks `entry`
    // from its current list and inserts it immediately after `head`.
    unsafe {
        __list_del((*entry).prev, (*entry).next);
        list_add(entry, head);
    }
}

/// # Safety
/// `entry` must be currently linked in a circular list, and `head` must be an initialized
/// circular list head. The caller must have exclusive access to `entry`, its current
/// neighbors, `head`, and `head.prev`.
pub unsafe fn list_move_tail(entry: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `entry` and `head` are valid and exclusively accessed; unlinks `entry`
    // from its current list and inserts it at the tail (before `head`).
    unsafe {
        __list_del((*entry).prev, (*entry).next);
        list_add_tail(entry, head);
    }
}

/// Move all entries from `list` to the beginning of `head`, then reinitialize
/// `list`, matching Linux list_splice_init().
/// # Safety
/// `list` and `head` must be non-null, aligned pointers to initialized circular
/// `ListHead`s. The caller must hold exclusive access to both lists and their elements.
/// `list` is emptied and reinitialized.
pub unsafe fn list_splice_init(list: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `list` is non-null and aligned; creates a temporary shared reference to test whether it is empty.
    if unsafe { list_empty(&*list) } {
        return;
    }
    // SAFETY: list is non-empty; read first node pointer to attach after head.
    let first = unsafe { (*list).next };
    // SAFETY: list is non-empty; read last node pointer to attach before head.next.
    let last = unsafe { (*list).prev };
    // SAFETY: head is initialized; read head.next to determine splice insertion point.
    let at = unsafe { (*head).next };
    // SAFETY: splice detached list nodes between head and head.next, then reinitialize list to empty.
    unsafe {
        (*first).prev = head;
        (*head).next = first;
        (*last).next = at;
        (*at).prev = last;
        INIT_LIST_HEAD(list);
    }
}

/// Insert all entries from `list` after `head` without reinitializing `list`,
/// matching Linux `list_splice()` (`include/linux/list.h`).
#[inline]
/// # Safety
/// `list` and `head` must be non-null, aligned pointers to initialized circular
/// `ListHead`s. The caller must hold exclusive access to both lists and their elements.
pub unsafe fn list_splice(list: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `list` is non-null and aligned; creates a temporary shared reference to test whether it is empty.
    if unsafe { list_empty(&*list) } {
        return;
    }
    // SAFETY: list is non-empty; read its first node to attach after head.
    let first = unsafe { (*list).next };
    // SAFETY: list is non-empty; read its last node to attach before head.next.
    let last = unsafe { (*list).prev };
    // SAFETY: head is initialized; read head.next as the insertion boundary.
    let at = unsafe { (*head).next };
    // SAFETY: splice nodes between head and head.next; list head is left detached without reinitialization.
    unsafe {
        (*first).prev = head;
        (*head).next = first;
        (*last).next = at;
        (*at).prev = last;
    }
}

/// Insert all entries from `list` immediately before `head`, matching Linux
/// `list_splice_tail()` without reinitializing the source list.
#[inline]
/// # Safety
/// `list` and `head` must be non-null, aligned pointers to initialized circular
/// `ListHead`s. The caller must hold exclusive access to both lists and their elements.
pub unsafe fn list_splice_tail(list: *mut ListHead, head: *mut ListHead) {
    // SAFETY: `list` is non-null and aligned; creates a temporary shared reference to test whether it is empty.
    if unsafe { list_empty(&*list) } {
        return;
    }
    // SAFETY: list is non-empty; read its first node to attach after head.prev.
    let first = unsafe { (*list).next };
    // SAFETY: list is non-empty; read its last node to attach before head.
    let last = unsafe { (*list).prev };
    // SAFETY: head is initialized; read head.prev to find current list tail.
    let prev = unsafe { (*head).prev };
    // SAFETY: splice nodes between head.prev and head; list head is left detached without reinitialization.
    unsafe {
        (*prev).next = first;
        (*first).prev = prev;
        (*last).next = head;
        (*head).prev = last;
    }
}

/// # Safety
/// `old` must be a currently linked node in a circular list, and `new` must be
/// an unlinked node. Exclusive access to `old`, `new`, and `old`'s adjacent neighbors
/// is required.
pub unsafe fn list_replace(old: *mut ListHead, new: *mut ListHead) {
    // SAFETY: `old` is linked and `new` is unlinked; splices `new` in place of `old`
    // by copying link pointers and updating adjacent nodes.
    unsafe {
        (*new).next = (*old).next;
        (*(*new).next).prev = new;
        (*new).prev = (*old).prev;
        (*(*new).prev).next = new;
    }
}

/// # Safety
/// `head` must be an initialized circular list head. `new` must be an unlinked
/// node. The caller must hold the writer lock for the list. Concurrent RCU readers
/// may traverse `head`.
pub unsafe fn list_add_rcu(new: *mut ListHead, head: *mut ListHead) {
    // SAFETY: writer holds exclusive modification rights; initializes `new->next`
    // and `new->prev`, executes release fence, then updates `head->next` and `next->prev`
    // so concurrent RCU readers see fully initialized link pointers.
    unsafe {
        let next = (*head).next;
        (*new).next = next;
        (*new).prev = head;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        (*head).next = new;
        (*next).prev = new;
    }
}

/// Add an entry to the tail of an RCU-visible list, preserving the kernel's
/// publication order for readers that traverse from the head.
#[inline]
/// # Safety
/// `head` must be an initialized circular list head. `new` must be an unlinked
/// node. The caller must hold writer synchronization for the list. Concurrent RCU
/// readers may traverse from `head`.
pub unsafe fn list_add_tail_rcu(new: *mut ListHead, head: *mut ListHead) {
    // SAFETY: writer holds exclusive modification rights; initializes `new` links,
    // executes release fence, and links `new` before `head` with release semantics.
    unsafe {
        let prev = (*head).prev;
        (*new).next = head;
        (*new).prev = prev;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        (*prev).next = new;
        (*head).prev = new;
    }
}

/// # Safety
/// `entry` must be a currently linked node in an RCU-protected circular list.
/// The caller must hold writer synchronization. The storage of `entry` must not
/// be reclaimed until an RCU grace period elapses.
pub unsafe fn list_del_rcu(entry: *mut ListHead) {
    // SAFETY: writer holds exclusive update rights; unlinks `entry` from `prev` and `next`,
    // orders memory with release fence, and poisons only `entry->prev` so concurrent readers
    // can still traverse `entry->next` safely.
    unsafe {
        let next = (*entry).next;
        let prev = (*entry).prev;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        (*prev).next = next;
        (*next).prev = prev;
        (*entry).prev = 0xdead_0000_0000_0122usize as *mut ListHead;
    }
}

/// # Safety
/// `entry` and `head` must be non-null, aligned pointers to `ListHead` nodes in
/// valid storage with no conflicting concurrent write.
pub unsafe fn list_is_first(entry: *const ListHead, head: *const ListHead) -> bool {
    // SAFETY: `entry` is valid and readable; checks if its `prev` pointer matches `head`.
    unsafe { core::ptr::eq((*entry).prev, head as *mut ListHead) }
}

/// # Safety
/// `entry` and `head` must be non-null, aligned pointers to `ListHead` nodes in
/// valid storage with no conflicting concurrent write.
pub unsafe fn list_is_last(entry: *const ListHead, head: *const ListHead) -> bool {
    // SAFETY: `entry` is valid and readable; checks if its `next` pointer matches `head`.
    unsafe { core::ptr::eq((*entry).next, head as *mut ListHead) }
}

/// # Safety
/// `entry` and `head` must be valid, aligned pointers. `entry` must remain valid
/// for the read (e.g. within an RCU read-side critical section).
pub unsafe fn list_is_last_rcu(entry: *const ListHead, head: *const ListHead) -> bool {
    // SAFETY: `entry` points to a valid `ListHead` whose `next` pointer is sampled
    // via volatile read to observe RCU updates.
    unsafe {
        core::ptr::eq(
            core::ptr::read_volatile(core::ptr::addr_of!((*entry).next)) as *const ListHead,
            head,
        )
    }
}

/// # Safety
/// `head` must be a valid pointer to an initialized circular list head. Nodes in
/// the list must remain valid, initialized, and circular during counting.
pub unsafe fn list_count_nodes(head: *const ListHead) -> u32 {
    // SAFETY: `head` is a valid circular list head; traverses nodes using volatile
    // reads until returning to `head`.
    unsafe {
        let mut count = 0;
        let mut node = core::ptr::read_volatile(core::ptr::addr_of!((*head).next));
        while !core::ptr::eq(node, head as *mut ListHead) {
            count += 1;
            node = core::ptr::read_volatile(core::ptr::addr_of!((*node).next));
        }
        count
    }
}

/// `llist_empty()` from include/linux/llist.h; this is a single-copy head
/// pointer sample and, like Linux, is only a momentary observation.
/// # Safety
/// `head` must be a non-null, aligned pointer to an initialized `LlistHead`.
pub unsafe fn llist_empty(head: *const LlistHead) -> bool {
    assert!(!head.is_null());
    // SAFETY: head is non-null and valid as asserted; volatile read checks if first pointer is null.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*head).first)) }.is_null()
}

/// Compute a field offset for an inferred pointer type without reading the
/// uninitialized pointer slot. The temporary is valid MaybeUninit storage.
/// # Safety
/// `field` must calculate the address of a member within `T` using raw pointer
/// projections (`addr_of!`) without creating intermediate references to uninitialized memory.
pub unsafe fn offset_from_slot<T, U>(
    slot: *mut *mut T,
    field: impl FnOnce(*const T) -> *const U,
) -> usize {
    let _ = slot;
    let storage = MaybeUninit::<T>::uninit();
    let base = storage.as_ptr();
    (field(base) as usize).wrapping_sub(base as usize)
}

/// Recover the containing entry from a list node and its byte offset.
///
/// Keep the subtraction on a byte pointer: applying a byte offset directly to
/// `*mut ListHead` scales it by `size_of::<ListHead>()`.
#[inline]
pub fn entry_from_link<T>(link: *mut ListHead, offset: usize) -> *mut T {
    link.cast::<u8>().wrapping_sub(offset).cast::<T>()
}

#[macro_export]
macro_rules! INIT_LIST_HEAD {
    // SAFETY: `$head` is cast to `*mut ListHead` and passed to the unsafe `INIT_LIST_HEAD`; the caller must supply writable, aligned storage for a `ListHead`.
    ($head:expr) => {{ unsafe { $crate::list::INIT_LIST_HEAD($head as *mut $crate::types::ListHead) } }};
}

#[macro_export]
macro_rules! list_entry {
    ($pointer:expr, $container:ty, $($member:tt)+) => {{
        container_of!($pointer, $container, $($member)+)
    }};
}

#[macro_export]
macro_rules! llist_entry {
    ($pointer:expr, $container:ty, $member:ident $(.$member_rest:ident)*) => {{
        container_of!($pointer, $container, $member $(.$member_rest)*)
    }};
}

#[macro_export]
macro_rules! list_first_entry {
    ($head:expr, $container:ty, $($member:tt)+) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: head points to a valid non-empty list; (*head).next points to the link member of $container.
        unsafe { container_of!((*head).next, $container, $($member)+) }
    }};
}

#[macro_export]
macro_rules! list_last_entry {
    ($head:expr, $container:ty, $($member:tt)+) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: head points to a valid non-empty list; (*head).prev points to the link member of $container.
        unsafe { container_of!((*head).prev, $container, $($member)+) }
    }};
}

#[macro_export]
macro_rules! list_first_entry_or_null {
    ($head:expr, $container:ty, $($member:tt)+) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: head points to an initialized circular list; check if empty, or recover container of first entry.
        if unsafe { core::ptr::eq((*head).next, head) } { core::ptr::null_mut() }
        // SAFETY: `(*head).next` is read only on the non-empty branch, so it names the live link member of an entry in the list.
        else { unsafe { container_of!((*head).next, $container, $($member)+) } }
    }};
}

#[macro_export]
macro_rules! list_for_each_entry {
    ($pos:ident, $head:expr, $member:ident $(.$member_rest:ident)*, $body:block) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: computes forward iterator member byte offset for $member in $container using uninit storage.
        let offset = unsafe {
            $crate::list::offset_from_slot(
                core::ptr::addr_of_mut!($pos),
                |base| core::ptr::addr_of!((*base).$member $(.$member_rest)*),
            )
        };
        // SAFETY: head is a valid ListHead pointer; volatile read fetches the first list element.
        let mut cursor = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*head).next)) };
        while !core::ptr::eq(cursor, head) {
            let current = cursor;
            // SAFETY: cursor is a valid node within the list; volatile read fetches next element link.
            cursor = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).next)) };
            $pos = $crate::list::entry_from_link(current, offset);
            $body
        }
    }};
}

#[macro_export]
macro_rules! list_for_each_entry_safe {
    ($pos:ident, $tmp:ident, $head:expr, $member:ident $(.$member_rest:ident)*, $body:block) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: computes safe forward iterator member byte offset for $member in $container using uninit storage.
        let offset = unsafe {
            $crate::list::offset_from_slot(
                core::ptr::addr_of_mut!($pos),
                |base| core::ptr::addr_of!((*base).$member $(.$member_rest)*),
            )
        };
        // SAFETY: head is a valid ListHead pointer; volatile read fetches the first list element.
        let mut cursor = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*head).next)) };
        while !core::ptr::eq(cursor, head) {
            let current = cursor;
            // SAFETY: cursor is a valid node within the list; volatile read snapshots next link before user body execution.
            let next = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).next)) };
            cursor = next;
            $pos = $crate::list::entry_from_link(current, offset);
            $tmp = $crate::list::entry_from_link(next, offset);
            $body
        }
    }};
}

// Scheduler list traversal adapts to the request link embedded in a
// priolist. The safe form snapshots the next list node before the caller may
// remove the current request, matching list_for_each_entry_safe().
#[macro_export]
macro_rules! list_for_each_entry_reverse {
    ($pos:ident, $head:expr, $member:ident $(.$member_rest:ident)*, $body:block) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: computes reverse iterator member byte offset for $member in $container using uninit storage.
        let offset = unsafe {
            $crate::list::offset_from_slot(
                core::ptr::addr_of_mut!($pos),
                |base| core::ptr::addr_of!((*base).$member $(.$member_rest)*),
            )
        };
        // SAFETY: head is a valid ListHead pointer; volatile read fetches previous element link.
        let mut cursor: *mut $crate::types::ListHead = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!((*head).prev))
        };
        while !core::ptr::eq(cursor, head) {
            let current = cursor;
            // SAFETY: cursor is a valid node within the list; volatile read fetches previous link.
            cursor = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).prev)) };
            $pos = $crate::list::entry_from_link(current, offset);
            $body
        }
    }};
}

#[macro_export]
macro_rules! list_for_each_entry_safe_reverse {
    ($pos:ident, $tmp:ident, $container:ty, $head:expr, $member:ident $(.$member_rest:ident)*, $body:block) => {{
        // Linux's C list macro declares the iteration objects at the callsite;
        // the Rust source-order form has no `let` syntax at those sites.
        let mut $pos: *mut $container = core::ptr::null_mut();
        let mut $tmp: *mut $container = core::ptr::null_mut();
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: computes safe-reverse iterator member byte offset for $member in $container using uninit storage.
        let offset = unsafe {
            $crate::list::offset_from_slot(
                core::ptr::addr_of_mut!($pos),
                |base| core::ptr::addr_of!((*base).$member $(.$member_rest)*),
            )
        };
        // SAFETY: head is a valid ListHead pointer; volatile read fetches previous element link.
        let mut cursor: *mut $crate::types::ListHead = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!((*head).prev))
        };
        while !core::ptr::eq(cursor, head) {
            let current = cursor;
            // SAFETY: cursor is a valid node within the list; volatile read snapshots previous link before user body execution.
            let prev = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).prev)) };
            cursor = prev;
            $pos = $crate::list::entry_from_link(current, offset);
            $tmp = $crate::list::entry_from_link(prev, offset);
            $body
        }
    }};
}

#[macro_export]
macro_rules! list_for_each_entry_from_reverse {
    ($pos:ident, $head:expr, $member:ident $(.$member_rest:ident)*, $body:block) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        // SAFETY: computes from-reverse iterator member byte offset for $member in $container using uninit storage.
        let offset = unsafe {
            $crate::list::offset_from_slot(
                core::ptr::addr_of_mut!($pos),
                |base| core::ptr::addr_of!((*base).$member $(.$member_rest)*),
            )
        };
        // SAFETY: $pos is a valid container pointer whose $member is an embedded ListHead link.
        let mut cursor = unsafe { core::ptr::addr_of_mut!((*$pos).$member $(.$member_rest)*) } as *mut $crate::types::ListHead;
        while !core::ptr::eq(cursor, head) {
            let current = cursor;
            // SAFETY: cursor is a valid node within the list; volatile read fetches previous link.
            cursor = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).prev)) };
            $pos = $crate::list::entry_from_link(current, offset);
            $body
        }
    }};
}

#[macro_export]
macro_rules! list_for_each_entry_rcu {
    ($pos:ident, $head:expr, $member:ident $(.$member_rest:ident)*, $body:block) => {
        list_for_each_entry!($pos, $head, $member $(.$member_rest)*, $body)
    };
}

#[macro_export]
macro_rules! list_for_each_prev {
    ($pos:ident, $head:expr, $body:block) => {{
        let head = ($head) as *const _ as *mut $crate::types::ListHead;
        let mut cursor: *mut $crate::types::ListHead =
            // SAFETY: head is a valid ListHead pointer; volatile read fetches previous element link.
            unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*head).prev)) };
        while !core::ptr::eq(cursor, head) {
            $pos = cursor;
            // SAFETY: cursor is a valid node within the list; volatile read fetches previous link for reverse step.
            cursor = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).prev)) };
            $body
        }
    }};
}

#[macro_export]
macro_rules! llist_for_each_safe {
    ($pos:ident, $tmp:ident, $head:ident, $body:block) => {{
        let mut cursor = $head;
        while !cursor.is_null() {
            // SAFETY: cursor is non-null and points to a valid LlistNode; volatile read fetches next pointer.
            let next = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*cursor).next)) };
            $pos = cursor;
            $tmp = next;
            cursor = next;
            $body
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct Entry {
        link: ListHead,
        key: u32,
        serial: u32,
    }

    #[repr(C)]
    struct OffsetEntry {
        prefix: [u64; 3],
        link: ListHead,
        value: u32,
    }

    /// # Safety
    /// `a` and `b` must point to `ListHead` fields embedded at offset 0 within
    /// valid, initialized, and readable `Entry` structs.
    unsafe extern "C" fn by_key(
        _priv: *mut core::ffi::c_void,
        a: *const ListHead,
        b: *const ListHead,
    ) -> i32 {
        let a = a.cast::<Entry>();
        let b = b.cast::<Entry>();
        // SAFETY: a and b point to valid Entry instances whose link field is at offset 0.
        unsafe { (*a).key.cmp(&(*b).key) as i32 }
    }

    #[test]
    fn list_sort_is_stable_and_restores_both_links() {
        let mut head = ListHead {
            next: core::ptr::null_mut(),
            prev: core::ptr::null_mut(),
        };
        // SAFETY: head is a local stack ListHead variable valid for exclusive mutation.
        unsafe { INIT_LIST_HEAD(&mut head) };
        let mut entries = [
            Entry {
                link: ListHead {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
                key: 2,
                serial: 0,
            },
            Entry {
                link: ListHead {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
                key: 1,
                serial: 1,
            },
            Entry {
                link: ListHead {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
                key: 2,
                serial: 2,
            },
        ];
        for entry in &mut entries {
            // SAFETY: entry.link is a valid stack-allocated ListHead linked to head.
            unsafe { list_add_tail(core::ptr::addr_of_mut!(entry.link), &mut head) };
        }

        // SAFETY: head is an initialized circular list containing entries array elements.
        unsafe { list_sort(core::ptr::null_mut(), &mut head, by_key) };

        let mut got = [(0u32, 0u32); 3];
        let mut link = head.next;
        for item in &mut got {
            assert!(!core::ptr::eq(link, &mut head));
            let entry = link.cast::<Entry>();
            // SAFETY: entry points to a valid stack Entry within entries array.
            *item = unsafe { ((*entry).key, (*entry).serial) };
            // SAFETY: link and its next/prev pointers are valid nodes in the circular list.
            assert!(core::ptr::eq(unsafe { (*(*link).next).prev }, link));
            // SAFETY: link is a valid node; reading link.next traverses to the next entry.
            link = unsafe { (*link).next };
        }
        assert!(core::ptr::eq(link, &mut head));
        assert_eq!(got, [(1, 1), (2, 0), (2, 2)]);
    }

    #[test]
    fn list_iteration_macros_use_byte_offsets_for_embedded_links() {
        assert_ne!(core::mem::offset_of!(OffsetEntry, link), 0);

        let mut head = ListHead {
            next: core::ptr::null_mut(),
            prev: core::ptr::null_mut(),
        };
        // SAFETY: head is a local stack ListHead variable valid for exclusive mutation.
        unsafe { INIT_LIST_HEAD(&mut head) };
        let mut entries = [
            OffsetEntry {
                prefix: [0; 3],
                link: ListHead {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
                value: 10,
            },
            OffsetEntry {
                prefix: [0; 3],
                link: ListHead {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
                value: 20,
            },
            OffsetEntry {
                prefix: [0; 3],
                link: ListHead {
                    next: core::ptr::null_mut(),
                    prev: core::ptr::null_mut(),
                },
                value: 30,
            },
        ];
        for entry in &mut entries {
            // SAFETY: entry.link is a valid stack-allocated ListHead linked to head.
            unsafe { list_add_tail(core::ptr::addr_of_mut!(entry.link), &mut head) };
        }

        let mut forward = [0; 3];
        let mut forward_count = 0;
        let mut pos: *mut OffsetEntry = core::ptr::null_mut();
        list_for_each_entry!(pos, &mut head, link, {
            // SAFETY: pos is non-null and points to an OffsetEntry in the forward traversal pass.
            forward[forward_count] = unsafe { (*pos).value };
            forward_count += 1;
        });
        // Keep the macro's final cursor update observable in host test builds.
        core::hint::black_box(pos);
        assert_eq!(forward_count, forward.len());
        assert_eq!(forward, [10, 20, 30]);

        let mut reverse = [0; 3];
        let mut reverse_count = 0;
        let mut pos: *mut OffsetEntry = core::ptr::null_mut();
        list_for_each_entry_reverse!(pos, &mut head, link, {
            // SAFETY: pos is non-null and points to an OffsetEntry in the reverse traversal pass.
            reverse[reverse_count] = unsafe { (*pos).value };
            reverse_count += 1;
        });
        core::hint::black_box(pos);
        assert_eq!(reverse_count, reverse.len());
        assert_eq!(reverse, [30, 20, 10]);

        let mut removed = [0; 3];
        let mut removed_count = 0;
        let mut pos: *mut OffsetEntry = core::ptr::null_mut();
        let mut next: *mut OffsetEntry = core::ptr::null_mut();
        list_for_each_entry_safe!(pos, next, &mut head, link, {
            // SAFETY: pos is non-null and points to an OffsetEntry in the safe forward deletion pass.
            removed[removed_count] = unsafe { (*pos).value };
            removed_count += 1;
            // SAFETY: pos points to a valid OffsetEntry whose link is unlinked from head.
            unsafe { list_del(core::ptr::addr_of_mut!((*pos).link)) };
        });
        core::hint::black_box(pos);
        core::hint::black_box(next);
        assert_eq!(removed_count, removed.len());
        assert_eq!(removed, [10, 20, 30]);
        assert!(core::ptr::eq(head.next, &mut head));
        assert!(core::ptr::eq(head.prev, &mut head));
    }

    #[test]
    fn list_iteration_macros_leave_empty_lists_empty() {
        let mut head = ListHead {
            next: core::ptr::null_mut(),
            prev: core::ptr::null_mut(),
        };
        // SAFETY: head is a local stack ListHead variable valid for exclusive mutation.
        unsafe { INIT_LIST_HEAD(&mut head) };
        let mut visits = 0;

        let mut pos: *mut OffsetEntry = core::ptr::null_mut();
        list_for_each_entry!(pos, &mut head, link, {
            visits += 1;
        });
        core::hint::black_box(pos);
        let mut pos: *mut OffsetEntry = core::ptr::null_mut();
        list_for_each_entry_reverse!(pos, &mut head, link, {
            visits += 1;
        });
        core::hint::black_box(pos);
        let mut pos: *mut OffsetEntry = core::ptr::null_mut();
        let mut next: *mut OffsetEntry = core::ptr::null_mut();
        list_for_each_entry_safe!(pos, next, &mut head, link, {
            visits += 1;
        });
        core::hint::black_box(pos);
        core::hint::black_box(next);

        assert_eq!(visits, 0);
        assert!(core::ptr::eq(head.next, &mut head));
        assert!(core::ptr::eq(head.prev, &mut head));
    }
}
