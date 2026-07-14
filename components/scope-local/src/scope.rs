use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, iter::zip, mem::MaybeUninit, ptr::NonNull};

use ax_kspin::PreemptGuard;
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
        let guard = PreemptGuard::new();
        // SAFETY: the public contract supplies the scope lifetime and aliasing
        // invariants; the guard supplies the current-CPU pin.
        unsafe { Self::set_pinned(scope, guard.cpu_pin()) };
    }

    /// Sets the active scope while borrowing an existing CPU pin.
    ///
    /// This variant performs no context transition and is therefore suitable
    /// for scheduler and hard-IRQ code that already owns a pin.
    ///
    /// # Safety
    ///
    /// The caller must keep `scope` alive for every current-CPU access until a
    /// later [`Self::set_global_pinned`] or pinned replacement, and must prevent
    /// concurrent mutable access to that scope's items.
    pub unsafe fn set_pinned(scope: &Scope, pin: &CpuPin) {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.write_current(&bound_pin, scope.ptr.addr().into());
    }

    /// Set the active scope to the global scope.
    pub fn set_global() {
        let guard = PreemptGuard::new();
        Self::set_global_pinned(guard.cpu_pin());
    }

    /// Sets the active scope to the global scope under an existing CPU pin.
    pub fn set_global_pinned(pin: &CpuPin) {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.write_current(&bound_pin, 0);
    }

    /// Returns true if the active scope is the global scope.
    pub fn is_global() -> bool {
        let guard = PreemptGuard::new();
        Self::is_global_pinned(guard.cpu_pin())
    }

    /// Returns true if the active scope is global under an existing CPU pin.
    pub fn is_global_pinned(pin: &CpuPin) -> bool {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.read_current(&bound_pin) == 0
    }

    pub(crate) fn with_item<R>(
        item: &'static Item,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> R {
        operation(Self::current_slot(item, pin).get())
    }

    pub(crate) fn try_with_item<R>(
        item: &'static Item,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> Option<R> {
        Self::try_current_slot(item, pin)?.try_get().map(operation)
    }

    fn current_slot<'pin>(item: &'static Item, pin: &'pin CpuPin) -> &'pin ItemSlot {
        let bound_pin = bound_current(pin);
        let ptr = ACTIVE_SCOPE_PTR.read_current(&bound_pin);
        let ptr = NonNull::new(ptr as _).unwrap_or_else(|| GLOBAL_SCOPE.call_once(Scope::new).ptr);
        let index = item.index();
        // SAFETY: set_pinned's contract keeps the selected scope live and
        // prevents mutable aliasing. The returned borrow is shortened to the
        // CpuPin lifetime so it cannot survive migration through safe APIs.
        unsafe { ptr.add(index).as_ref() }
    }

    fn try_current_slot<'pin>(item: &'static Item, pin: &'pin CpuPin) -> Option<&'pin ItemSlot> {
        let bound_pin = bound_current(pin);
        let ptr = ACTIVE_SCOPE_PTR.read_current(&bound_pin);
        let ptr = match NonNull::new(ptr as _) {
            Some(ptr) => ptr,
            None => GLOBAL_SCOPE.get()?.ptr,
        };
        let index = item.index();
        // SAFETY: the same scope lifetime and pinning invariants as
        // current_slot apply. Unlike that path, GLOBAL_SCOPE.get never runs an
        // initializer and therefore remains valid in hard-IRQ context.
        Some(unsafe { ptr.add(index).as_ref() })
    }
}

fn bound_current(pin: &CpuPin) -> BoundCpuPin<'_> {
    ax_percpu::bound_current(pin).expect("scope-local access requires a bound CPU-local area")
}
