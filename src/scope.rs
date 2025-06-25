use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, iter::zip, mem::MaybeUninit, ptr::NonNull};

use spin::Lazy;

use crate::{
    boxed::ItemBox,
    item::{Item, Registry},
};

/// A scope is a collection of items.
pub struct Scope {
    // Not using [ItemBox<A>] to save a `usize` because we know the length
    ptr: NonNull<ItemBox>,
}

unsafe impl Send for Scope {}
unsafe impl Sync for Scope {}

impl Scope {
    fn layout() -> Layout {
        Layout::array::<ItemBox>(Registry.len()).unwrap()
    }

    /// Create a new namespace with all resources initialized as their default
    /// value.
    pub fn new() -> Self {
        let layout = Self::layout();
        let ptr = NonNull::new(unsafe { alloc(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout))
            .cast();

        let slice = unsafe {
            core::slice::from_raw_parts_mut(ptr.cast::<MaybeUninit<_>>().as_ptr(), Registry.len())
        };
        for (item, d) in zip(&*Registry, slice) {
            d.write(ItemBox::new(item));
        }

        Self { ptr }
    }

    pub(crate) fn get(&self, item: &'static Item) -> &ItemBox {
        let index = item.index();
        unsafe { self.ptr.add(index).as_ref() }
    }

    pub(crate) fn get_mut(&mut self, item: &'static Item) -> &mut ItemBox {
        let index = item.index();
        unsafe { self.ptr.add(index).as_mut() }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let ptr = NonNull::slice_from_raw_parts(self.ptr, Registry.len());
        unsafe {
            ptr.drop_in_place();
            dealloc(self.ptr.cast().as_ptr(), Self::layout());
        }
    }
}

static GLOBAL_SCOPE: Lazy<Scope> = Lazy::new(Scope::new);

#[percpu::def_percpu]
pub(crate) static ACTIVE_SCOPE_PTR: usize = 0;

/// Currently active scope.
pub struct ActiveScope;

impl ActiveScope {
    /// Sets the active scope pointer to the given scope.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the provided `scope` reference is valid for
    /// the duration in which it is set as the active scope, and that no data
    /// races or aliasing violations occur.
    pub unsafe fn set(scope: &Scope) {
        ACTIVE_SCOPE_PTR.write_current(scope.ptr.addr().into());
    }

    /// Set the active scope to the global scope.
    pub fn set_global() {
        ACTIVE_SCOPE_PTR.write_current(0);
    }

    /// Returns true if the active scope is the global scope.
    pub fn is_global() -> bool {
        ACTIVE_SCOPE_PTR.read_current() == 0
    }

    pub(crate) fn get<'a>(item: &'static Item) -> &'a ItemBox {
        let ptr = ACTIVE_SCOPE_PTR.read_current();
        let ptr = NonNull::new(ptr as _).unwrap_or(GLOBAL_SCOPE.ptr);
        let index = item.index();
        unsafe { ptr.add(index).as_ref() }
    }
}
