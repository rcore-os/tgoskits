use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, iter::zip, mem::MaybeUninit, ptr::NonNull};

use ax_kernel_guard::NoPreempt;
use ax_percpu::{BoundCpuPin, CpuPin};
use spin::Once;

use crate::{
    boxed::ItemBox,
    item::{Item, Registry},
};

/// A scope is a collection of items.
pub struct Scope {
    ptr: NonNull<ItemSlot>,
}

unsafe impl Send for Scope {}
unsafe impl Sync for Scope {}

impl Scope {
    fn len() -> usize {
        Registry.len()
    }

    fn layout() -> Layout {
        Layout::array::<ItemSlot>(Self::len()).unwrap()
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
            d.write(ItemSlot::new(item));
        }

        Self { ptr }
    }

    pub(crate) fn get(&self, item: &'static Item) -> &ItemBox {
        let index = item.index();
        unsafe { self.ptr.add(index).as_ref() }.get()
    }

    pub(crate) fn get_mut(&mut self, item: &'static Item) -> &mut ItemBox {
        let index = item.index();
        unsafe { self.ptr.add(index).as_mut() }.get_mut()
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let ptr = NonNull::slice_from_raw_parts(self.ptr, Self::len());
        unsafe {
            ptr.drop_in_place();
            dealloc(self.ptr.cast().as_ptr(), Self::layout());
        }
    }
}

struct ItemSlot {
    item: &'static Item,
    value: Once<ItemBox>,
}

impl ItemSlot {
    fn new(item: &'static Item) -> Self {
        Self {
            item,
            value: Once::new(),
        }
    }

    fn get(&self) -> &ItemBox {
        self.value.call_once(|| ItemBox::new(self.item))
    }

    fn get_mut(&mut self) -> &mut ItemBox {
        if !self.value.is_completed() {
            let item = self.item;
            self.value.call_once(|| ItemBox::new(item));
        }
        self.value
            .get_mut()
            .expect("scope-local item must be initialized")
    }

    fn try_get(&self) -> Option<&ItemBox> {
        self.value.get()
    }
}

static GLOBAL_SCOPE: Once<Scope> = Once::new();

#[ax_percpu::def_percpu]
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
        let _guard = NoPreempt::new();
        // SAFETY: the guard prevents migration while the per-CPU pointer is
        // selected and updated.
        let pin = unsafe { CpuPin::new_unchecked() };
        unsafe { Self::set_pinned(scope, &pin) };
    }

    /// Sets the active scope under an existing CPU pin.
    ///
    /// # Safety
    ///
    /// `scope` must remain alive until a later pinned replacement or reset.
    pub unsafe fn set_pinned(scope: &Scope, pin: &CpuPin) {
        let bound = bound_current(pin);
        ACTIVE_SCOPE_PTR.write_current(&bound, scope.ptr.addr().get());
    }

    /// Set the active scope to the global scope.
    pub fn set_global() {
        let _guard = NoPreempt::new();
        // SAFETY: the guard prevents migration while the per-CPU pointer is
        // cleared.
        let pin = unsafe { CpuPin::new_unchecked() };
        Self::set_global_pinned(&pin);
    }

    /// Sets the active scope to global under an existing CPU pin.
    pub fn set_global_pinned(pin: &CpuPin) {
        let bound = bound_current(pin);
        ACTIVE_SCOPE_PTR.write_current(&bound, 0);
    }

    /// Returns true if the active scope is the global scope.
    pub fn is_global() -> bool {
        let _guard = NoPreempt::new();
        // SAFETY: the guard prevents migration for the complete read.
        let pin = unsafe { CpuPin::new_unchecked() };
        Self::is_global_pinned(&pin)
    }

    /// Returns whether the active scope is global under an existing pin.
    pub fn is_global_pinned(pin: &CpuPin) -> bool {
        let bound = bound_current(pin);
        ACTIVE_SCOPE_PTR.read_current(&bound) == 0
    }

    pub(crate) fn with_item<R>(
        item: &'static Item,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> R {
        let bound = bound_current(pin);
        let ptr = ACTIVE_SCOPE_PTR.read_current(&bound);
        let ptr = NonNull::new(ptr as *mut ItemSlot)
            .unwrap_or_else(|| GLOBAL_SCOPE.call_once(Scope::new).ptr);
        let index = item.index();
        operation(unsafe { ptr.add(index).as_ref() }.get())
    }

    pub(crate) fn try_with_item<R>(
        item: &'static Item,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> Option<R> {
        let bound = bound_current(pin);
        let ptr = ACTIVE_SCOPE_PTR.read_current(&bound);
        let ptr = if ptr == 0 {
            GLOBAL_SCOPE.get()?.ptr
        } else {
            NonNull::new(ptr as *mut ItemSlot)?
        };
        let index = item.index();
        Some(operation(unsafe { ptr.add(index).as_ref() }.try_get()?))
    }
}

fn bound_current(pin: &CpuPin) -> BoundCpuPin<'_> {
    ax_percpu::bound_current(pin).expect("scope-local access requires a bound CPU-local area")
}
