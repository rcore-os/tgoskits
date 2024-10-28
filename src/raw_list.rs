// SPDX-License-Identifier: GPL-2.0

//! Raw lists.
//!
//! Copied from linux/rust/kernel/raw_list.rs.
//!
//! TODO: This module is a work in progress.

use core::{
    cell::UnsafeCell,
    iter, ptr,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

/// A descriptor of list elements.
///
/// It describes the type of list elements and provides a function to determine how to get the
/// links to be used on a list.
///
/// A type that may be in multiple lists simultaneously needs to implement one of these for each
/// simultaneous list.
pub trait GetLinks {
    /// The type of the entries in the list.
    type EntryType: ?Sized;

    /// Returns the links to be used when linking an entry within a list.
    fn get_links(data: &Self::EntryType) -> &Links<Self::EntryType>;
}

/// The links used to link an object on a linked list.
///
/// Instances of this type are usually embedded in structures and returned in calls to
/// [`GetLinks::get_links`].
pub struct Links<T: ?Sized> {
    inserted: AtomicBool,
    entry: UnsafeCell<ListEntry<T>>,
}

// SAFETY: `Links` can be safely sent to other threads but we restrict it to being `Send` only when
// the list entries it points to are also `Send`.
unsafe impl<T: ?Sized> Send for Links<T> {}

// SAFETY: `Links` is usable from other threads via references but we restrict it to being `Sync`
// only when the list entries it points to are also `Sync`.
unsafe impl<T: ?Sized> Sync for Links<T> {}

impl<T: ?Sized> Links<T> {
    /// Constructs a new [`Links`] instance that isn't inserted on any lists yet.
    pub const fn new() -> Self {
        Self {
            inserted: AtomicBool::new(false),
            entry: UnsafeCell::new(ListEntry::new()),
        }
    }

    fn acquire_for_insertion(&self) -> bool {
        self.inserted
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn release_after_removal(&self) {
        self.inserted.store(false, Ordering::Release);
    }
}

impl<T: ?Sized> Default for Links<T> {
    fn default() -> Self {
        Self::new()
    }
}

struct ListEntry<T: ?Sized> {
    next: Option<NonNull<T>>,
    prev: Option<NonNull<T>>,
}

impl<T: ?Sized> ListEntry<T> {
    const fn new() -> Self {
        Self {
            next: None,
            prev: None,
        }
    }
}

/// A linked list.
///
/// # Invariants
///
/// The links of objects added to a list are owned by the list.
pub struct RawList<G: GetLinks> {
    head: Option<NonNull<G::EntryType>>,
}

impl<G: GetLinks> RawList<G> {
    /// Constructs a new empty RawList.
    pub const fn new() -> Self {
        Self { head: None }
    }

    /// Returns an iterator for the list starting at the first entry.
    pub fn iter(&self) -> Iterator<'_, G> {
        Iterator::new(self.cursor_front(), self.cursor_back())
    }

    /// Returns whether the RawList is empty.
    pub const fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    fn insert_after_priv(
        &mut self,
        existing: &G::EntryType,
        new_entry: &mut ListEntry<G::EntryType>,
        new_ptr: Option<NonNull<G::EntryType>>,
    ) {
        {
            // SAFETY: It's safe to get the previous entry of `existing` because the list cannot
            // change.
            let existing_links = unsafe { &mut *G::get_links(existing).entry.get() };
            new_entry.next = existing_links.next;
            existing_links.next = new_ptr;
        }

        new_entry.prev = Some(NonNull::from(existing));

        // SAFETY: It's safe to get the next entry of `existing` because the list cannot change.
        let next_links =
            unsafe { &mut *G::get_links(new_entry.next.unwrap().as_ref()).entry.get() };
        next_links.prev = new_ptr;
    }

    /// Inserts the given object after `existing`.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `existing` points to a valid entry that is on the list.
    pub unsafe fn insert_after(&mut self, existing: &G::EntryType, new: &G::EntryType) -> bool {
        let links = G::get_links(new);
        if !links.acquire_for_insertion() {
            // Nothing to do if already inserted.
            return false;
        }

        // SAFETY: The links are now owned by the list, so it is safe to get a mutable reference.
        let new_entry = unsafe { &mut *links.entry.get() };
        self.insert_after_priv(existing, new_entry, Some(NonNull::from(new)));
        true
    }

    fn push_back_internal(&mut self, new: &G::EntryType, front: bool) -> bool {
        let links = G::get_links(new);
        if !links.acquire_for_insertion() {
            // Nothing to do if already inserted.
            return false;
        }

        // SAFETY: The links are now owned by the list, so it is safe to get a mutable reference.
        let new_entry = unsafe { &mut *links.entry.get() };
        let new_ptr = Some(NonNull::from(new));
        match self.back() {
            // SAFETY: `back` is valid as the list cannot change.
            Some(back) => {
                self.insert_after_priv(unsafe { back.as_ref() }, new_entry, new_ptr);
                // if push front, update head
                if front {
                    self.head = new_ptr;
                }
            }
            None => {
                self.head = new_ptr;
                new_entry.next = new_ptr;
                new_entry.prev = new_ptr;
            }
        }
        true
    }

    /// Adds the given object to the end (back) of the list.
    ///
    /// Rawlist will save the reference as node ptr.
    /// The caller must ensure the validity of the reference while it is on
    /// the linked list.
    pub unsafe fn push_back(&mut self, new: &G::EntryType) -> bool {
        self.push_back_internal(new, false)
    }

    /// Adds the given object to the first (front) of the list.
    ///
    /// Rawlist will save the reference as node ptr.
    /// The caller must ensure the validity of the reference while it is on
    /// the linked list.
    pub unsafe fn push_front(&mut self, new: &G::EntryType) -> bool {
        self.push_back_internal(new, true)
    }

    fn remove_internal(&mut self, data: &G::EntryType) -> bool {
        let links = G::get_links(data);

        // SAFETY: The links are now owned by the list, so it is safe to get a mutable reference.
        let entry = unsafe { &mut *links.entry.get() };
        let next = if let Some(next) = entry.next {
            next
        } else {
            // Nothing to do if the entry is not on the list.
            return false;
        };

        if ptr::eq(data, next.as_ptr()) {
            // We're removing the only element.
            self.head = None
        } else {
            // Update the head if we're removing it.
            if let Some(raw_head) = self.head {
                if ptr::eq(data, raw_head.as_ptr()) {
                    self.head = Some(next);
                }
            }

            // SAFETY: It's safe to get the previous entry because the list cannot change.
            unsafe { &mut *G::get_links(entry.prev.unwrap().as_ref()).entry.get() }.next =
                entry.next;

            // SAFETY: It's safe to get the next entry because the list cannot change.
            unsafe { &mut *G::get_links(next.as_ref()).entry.get() }.prev = entry.prev;
        }

        // Reset the links of the element we're removing so that we know it's not on any list.
        entry.next = None;
        entry.prev = None;
        links.release_after_removal();
        true
    }

    /// Removes the given entry.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `data` is either on this list or in no list. It being on another
    /// list leads to memory unsafety.
    pub unsafe fn remove(&mut self, data: &G::EntryType) -> bool {
        self.remove_internal(data)
    }

    fn pop_front_internal(&mut self) -> Option<NonNull<G::EntryType>> {
        let head = self.head?;
        // SAFETY: The head is on the list as we just got it from there and it cannot change.
        unsafe { self.remove(head.as_ref()) };
        Some(head)
    }

    /// Get and Remove the first element of the list.
    pub fn pop_front(&mut self) -> Option<NonNull<G::EntryType>> {
        self.pop_front_internal()
    }

    ///  Just Get and not remove the first element of the list.
    pub(crate) fn front(&self) -> Option<NonNull<G::EntryType>> {
        self.head
    }

    /// Just Get and not remove the last element of the list.
    pub(crate) fn back(&self) -> Option<NonNull<G::EntryType>> {
        // SAFETY: The links of head are owned by the list, so it is safe to get a reference.
        unsafe { &*G::get_links(self.head?.as_ref()).entry.get() }.prev
    }

    /// Returns a cursor starting on the first element of the list.
    pub(crate) fn cursor_front(&self) -> Cursor<'_, G> {
        Cursor::new(self, self.front())
    }

    /// Returns a cursor starting on the last element of the list.
    pub(crate) fn cursor_back(&self) -> Cursor<'_, G> {
        Cursor::new(self, self.back())
    }

    /// Returns a mut cursor starting on the first element of the list.
    pub fn cursor_front_mut(&mut self) -> CursorMut<'_, G> {
        CursorMut::new(self, self.front())
    }
}

struct CommonCursor<G: GetLinks> {
    cur: Option<NonNull<G::EntryType>>,
}

impl<G: GetLinks> CommonCursor<G> {
    const fn new(cur: Option<NonNull<G::EntryType>>) -> Self {
        Self { cur }
    }

    fn move_next(&mut self, list: &RawList<G>) {
        match self.cur.take() {
            None => self.cur = list.head,
            Some(cur) => {
                if let Some(head) = list.head {
                    // SAFETY: We have a shared ref to the linked list, so the links can't change.
                    let links = unsafe { &*G::get_links(cur.as_ref()).entry.get() };
                    if !ptr::addr_eq(links.next.unwrap().as_ptr(), head.as_ptr()) {
                        self.cur = links.next;
                    }
                }
            }
        }
    }

    fn move_prev(&mut self, list: &RawList<G>) {
        match list.head {
            None => self.cur = None,
            Some(head) => {
                let next = match self.cur.take() {
                    None => head,
                    Some(cur) => {
                        if ptr::addr_eq(cur.as_ptr(), head.as_ptr()) {
                            return;
                        }
                        cur
                    }
                };
                // SAFETY: There's a shared ref to the list, so the links can't change.
                let links = unsafe { &*G::get_links(next.as_ref()).entry.get() };
                self.cur = links.prev;
            }
        }
    }
}

// SAFETY: The list is itself can be safely sent to other threads but we restrict it to being `Send`
// only when its entries are also `Send`.
unsafe impl<G: GetLinks> Send for RawList<G> where G::EntryType: Send {}

// SAFETY: The list is itself usable from other threads via references but we restrict it to being
// `Sync` only when its entries are also `Sync`.
unsafe impl<G: GetLinks> Sync for RawList<G> where G::EntryType: Sync {}

/// A list cursor that allows traversing a linked list and inspecting elements.
pub(crate) struct Cursor<'a, G: GetLinks> {
    cursor: CommonCursor<G>,
    list: &'a RawList<G>,
}

impl<'a, G: GetLinks> Cursor<'a, G> {
    pub(crate) fn new(list: &'a RawList<G>, cur: Option<NonNull<G::EntryType>>) -> Self {
        Self {
            list,
            cursor: CommonCursor::new(cur),
        }
    }

    /// Returns the element the cursor is currently positioned on.
    pub(crate) fn current(&self) -> Option<&'a G::EntryType> {
        let cur = self.cursor.cur?;
        // SAFETY: Objects must be kept alive while on the list.
        Some(unsafe { &*cur.as_ptr() })
    }

    /// Moves the cursor to the next element.
    pub(crate) fn move_next(&mut self) {
        self.cursor.move_next(self.list);
    }

    /// Moves the cursor to the prev element.
    #[allow(dead_code)]
    pub(crate) fn move_prev(&mut self) {
        self.cursor.move_prev(self.list);
    }
}

pub struct CursorMut<'a, G: GetLinks> {
    cursor: CommonCursor<G>,
    list: &'a mut RawList<G>,
}

impl<'a, G: GetLinks> CursorMut<'a, G> {
    fn new(list: &'a mut RawList<G>, cur: Option<NonNull<G::EntryType>>) -> Self {
        Self {
            list,
            cursor: CommonCursor::new(cur),
        }
    }

    pub fn current(&mut self) -> Option<&mut G::EntryType> {
        let cur = self.cursor.cur?;
        // SAFETY: Objects must be kept alive while on the list.
        Some(unsafe { &mut *cur.as_ptr() })
    }

    /// Removes the entry the cursor is pointing to and advances the cursor to the next entry. It
    /// returns a raw pointer to the removed element (if one is removed).
    pub fn remove_current(&mut self) -> Option<NonNull<G::EntryType>> {
        let entry = self.cursor.cur?;
        self.cursor.move_next(self.list);
        // SAFETY: The entry is on the list as we just got it from there and it cannot change.
        unsafe { self.list.remove(entry.as_ref()) };
        Some(entry)
    }

    pub fn peek_next(&mut self) -> Option<&mut G::EntryType> {
        let mut new = CommonCursor::new(self.cursor.cur);
        new.move_next(self.list);
        // SAFETY: Objects must be kept alive while on the list.
        Some(unsafe { &mut *new.cur?.as_ptr() })
    }

    pub fn peek_prev(&mut self) -> Option<&mut G::EntryType> {
        let mut new = CommonCursor::new(self.cursor.cur);
        new.move_prev(self.list);
        // SAFETY: Objects must be kept alive while on the list.
        Some(unsafe { &mut *new.cur?.as_ptr() })
    }

    pub fn move_next(&mut self) {
        self.cursor.move_next(self.list);
    }

    #[allow(dead_code)]
    pub fn move_prev(&mut self) {
        self.cursor.move_prev(self.list);
    }
}

impl<'a, G: GetLinks> iter::IntoIterator for &'a RawList<G> {
    type Item = &'a G::EntryType;
    type IntoIter = Iterator<'a, G>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An iterator for the linked list.
pub struct Iterator<'a, G: GetLinks> {
    cursor_front: Cursor<'a, G>,
    cursor_back: Cursor<'a, G>,
}

impl<'a, G: GetLinks> Iterator<'a, G> {
    const fn new(cursor_front: Cursor<'a, G>, cursor_back: Cursor<'a, G>) -> Self {
        Self {
            cursor_front,
            cursor_back,
        }
    }
}

impl<'a, G: GetLinks> iter::Iterator for Iterator<'a, G> {
    type Item = &'a G::EntryType;

    fn next(&mut self) -> Option<Self::Item> {
        let ret = self.cursor_front.current()?;
        self.cursor_front.move_next();
        Some(ret)
    }
}

impl<G: GetLinks> iter::DoubleEndedIterator for Iterator<'_, G> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let ret = self.cursor_back.current()?;
        self.cursor_back.move_prev();
        Some(ret)
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::{boxed::Box, vec::Vec};

    struct Example {
        links: super::Links<Self>,
    }

    // SAFETY: This is the only adapter that uses `Example::links`.
    impl super::GetLinks for Example {
        type EntryType = Self;
        fn get_links(obj: &Self) -> &super::Links<Self> {
            &obj.links
        }
    }

    fn build_vector(size: usize) -> Vec<Box<Example>> {
        let mut v = Vec::new();
        v.reserve(size);
        for _ in 0..size {
            v.push(Box::new(Example {
                links: super::Links::new(),
            }));
        }
        v
    }

    #[track_caller]
    fn assert_list_contents(v: &[Box<Example>], list: &super::RawList<Example>) {
        let n = v.len();

        // Assert that the list is ok going forward.
        let mut count = 0;
        for (i, e) in list.iter().enumerate() {
            assert!(core::ptr::eq(e, &*v[i]));
            count += 1;
        }
        assert_eq!(count, n);

        // Assert that the list is ok going backwards.
        let mut count = 0;
        for (i, e) in list.iter().rev().enumerate() {
            assert!(core::ptr::eq(e, &*v[n - 1 - i]));
            count += 1;
        }
        assert_eq!(count, n);
    }

    #[track_caller]
    fn test_each_element(
        min_len: usize,
        max_len: usize,
        test: impl Fn(&mut Vec<Box<Example>>, &mut super::RawList<Example>, usize, Box<Example>),
    ) {
        for n in min_len..=max_len {
            for i in 0..n {
                let extra = Box::new(Example {
                    links: super::Links::new(),
                });
                let mut v = build_vector(n);
                let mut list = super::RawList::<Example>::new();

                // Build list.
                for j in 0..n {
                    // SAFETY: The entry was allocated above, it's not in any lists yet, is never
                    // moved, and outlives the list.
                    unsafe { list.push_back(&v[j]) };
                }

                // Call the test case.
                test(&mut v, &mut list, i, extra);

                // Check that the list is ok.
                assert_list_contents(&v, &list);
            }
        }
    }

    #[test]
    fn test_push_back() {
        const MAX: usize = 10;
        let v = build_vector(MAX);
        let mut list = super::RawList::<Example>::new();

        for n in 1..=MAX {
            // SAFETY: The entry was allocated above, it's not in any lists yet, is never moved,
            // and outlives the list.
            unsafe { list.push_back(&v[n - 1]) };
            assert_list_contents(&v[..n], &list);
        }
    }

    #[test]
    fn test_push_front() {
        const MAX: usize = 10;
        let v = build_vector(MAX);
        let mut list = super::RawList::<Example>::new();

        for n in 1..=MAX {
            // SAFETY: The entry was allocated above, it's not in any lists yet, is never moved,
            // and outlives the list.
            println!("push front: {}", MAX - n);
            unsafe { list.push_front(&v[MAX - n]) };
            assert_list_contents(&v[MAX - n..MAX], &list);
        }
    }

    #[test]
    fn test_one_removal() {
        test_each_element(1, 10, |v, list, i, _| {
            // Remove the i-th element.
            // SAFETY: The i-th element was added to the list above, and wasn't removed yet.
            unsafe { list.remove(&v[i]) };
            v.remove(i);
        });
    }

    #[test]
    fn test_one_insert_after() {
        test_each_element(1, 10, |v, list, i, extra| {
            // Insert after the i-th element.
            // SAFETY: The i-th element was added to the list above, and wasn't removed yet.
            // Additionally, the new element isn't in any list yet, isn't moved, and outlives
            // the list.
            unsafe { list.insert_after(&*v[i], &*extra) };
            v.insert(i + 1, extra);
        });
    }
}
