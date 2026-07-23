use alloc::{boxed::Box, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU8, Ordering},
};

use ax_kernel_guard::NoPreempt;
use ax_percpu::CpuPin;
use spin::Once;

use crate::{
    boxed::ItemBox,
    item::{Item, Registry},
};

/// A scope is a collection of items.
pub struct Scope {
    items: Box<[ItemBox]>,
}

impl Scope {
    /// Creates a new namespace and eagerly initializes every registered item.
    ///
    /// Initializers run in the caller's ordinary context. Once this function
    /// returns, pinned access to the scope performs no allocation or lazy
    /// initialization.
    pub fn new() -> Self {
        let items = Registry
            .iter()
            .map(ItemBox::new)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { items }
    }

    pub(crate) fn get(&self, item: &'static Item) -> &ItemBox {
        &self.items[item.index()]
    }

    pub(crate) fn get_mut(&mut self, item: &'static Item) -> &mut ItemBox {
        &mut self.items[item.index()]
    }

    fn items_ptr(&self) -> NonNull<ItemBox> {
        NonNull::new(self.items.as_ptr().cast_mut())
            .expect("scope-local registry must contain the accessed item")
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_SCOPE: Once<Scope> = Once::new();
static GLOBAL_SCOPE_STATE: AtomicU8 = AtomicU8::new(GlobalScopeState::Uninitialized as u8);

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum GlobalScopeState {
    Uninitialized,
    Initializing,
    Ready,
}

struct GlobalInitialization {
    published: bool,
}

impl GlobalInitialization {
    fn begin() -> Self {
        Self { published: false }
    }

    fn publish(mut self, scope: Scope) {
        GLOBAL_SCOPE.call_once(|| scope);
        GLOBAL_SCOPE_STATE.store(GlobalScopeState::Ready as u8, Ordering::Release);
        self.published = true;
    }
}

impl Drop for GlobalInitialization {
    fn drop(&mut self) {
        if !self.published {
            GLOBAL_SCOPE_STATE.store(GlobalScopeState::Uninitialized as u8, Ordering::Release);
        }
    }
}

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
        unsafe {
            ax_percpu::with_cpu_pin(|pin| Self::set_pinned(scope, pin))
                .expect("scope-local access requires an installed CPU area")
        };
    }

    /// Sets the active scope under an existing CPU pin.
    ///
    /// # Safety
    ///
    /// `scope` must remain alive until a later pinned replacement or reset.
    pub unsafe fn set_pinned(scope: &Scope, pin: &CpuPin<'_>) {
        ACTIVE_SCOPE_PTR.write_current(pin, scope.items_ptr().addr().get());
    }

    /// Set the active scope to the global scope.
    pub fn set_global() {
        let _guard = NoPreempt::new();
        // SAFETY: the guard prevents migration while the per-CPU pointer is
        // cleared.
        unsafe {
            ax_percpu::with_cpu_pin(Self::set_global_pinned)
                .expect("scope-local access requires an installed CPU area")
        };
    }

    /// Sets the active scope to global under an existing CPU pin.
    pub fn set_global_pinned(pin: &CpuPin<'_>) {
        ACTIVE_SCOPE_PTR.write_current(pin, 0);
    }

    /// Returns true if the active scope is the global scope.
    pub fn is_global() -> bool {
        let _guard = NoPreempt::new();
        // SAFETY: the guard prevents migration for the complete read.
        unsafe {
            ax_percpu::with_cpu_pin(Self::is_global_pinned)
                .expect("scope-local access requires an installed CPU area")
        }
    }

    /// Returns whether the active scope is global under an existing pin.
    pub fn is_global_pinned(pin: &CpuPin<'_>) -> bool {
        ACTIVE_SCOPE_PTR.read_current(pin) == 0
    }

    pub(crate) fn with_item<R>(
        item: &'static Item,
        pin: &CpuPin<'_>,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> R {
        Self::try_with_item(item, pin, operation).expect(
            "scope-local global scope is not initialized; use LocalItem::with before pinned access",
        )
    }

    pub(crate) fn try_with_item<R>(
        item: &'static Item,
        pin: &CpuPin<'_>,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> Option<R> {
        let ptr = ACTIVE_SCOPE_PTR.read_current(pin);
        let items = if ptr == 0 {
            GLOBAL_SCOPE.get()?.items_ptr()
        } else {
            NonNull::new(ptr as *mut ItemBox)?
        };
        let index = item.index();
        Some(operation(unsafe { items.add(index).as_ref() }))
    }

    pub(crate) fn initialize_global() {
        loop {
            match GLOBAL_SCOPE_STATE.load(Ordering::Acquire) {
                state if state == GlobalScopeState::Ready as u8 => return,
                state if state == GlobalScopeState::Initializing as u8 => {
                    panic!("scope-local global scope initialization is already in progress")
                }
                state if state == GlobalScopeState::Uninitialized as u8 => {
                    if GLOBAL_SCOPE_STATE
                        .compare_exchange(
                            GlobalScopeState::Uninitialized as u8,
                            GlobalScopeState::Initializing as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        let initialization = GlobalInitialization::begin();
                        initialization.publish(Scope::new());
                        return;
                    }
                }
                _ => panic!("scope-local global scope has an invalid initialization state"),
            }
        }
    }
}
