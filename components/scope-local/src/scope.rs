use alloc::{boxed::Box, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
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
static GLOBAL_SCOPE_STATE: AtomicUsize = AtomicUsize::new(GlobalScopeState::Uninitialized as usize);

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(usize)]
enum GlobalScopeState {
    Uninitialized,
    Ready,
}

struct GlobalInitialization {
    owner_context: usize,
    published: bool,
}

impl GlobalInitialization {
    fn begin(owner_context: usize) -> Self {
        Self {
            owner_context,
            published: false,
        }
    }

    fn publish(mut self, scope: Scope) {
        GLOBAL_SCOPE.call_once(|| scope);
        GLOBAL_SCOPE_STATE.store(GlobalScopeState::Ready as usize, Ordering::Release);
        self.published = true;
    }
}

impl Drop for GlobalInitialization {
    fn drop(&mut self) {
        if !self.published {
            let _ = GLOBAL_SCOPE_STATE.compare_exchange(
                self.owner_context,
                GlobalScopeState::Uninitialized as usize,
                Ordering::Release,
                Ordering::Relaxed,
            );
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
        let owner_context = current_context_identity();
        loop {
            match GLOBAL_SCOPE_STATE.load(Ordering::Acquire) {
                state if state == GlobalScopeState::Ready as usize => return,
                state if state == owner_context => {
                    panic!("scope-local global scope initialization is already in progress")
                }
                state if state == GlobalScopeState::Uninitialized as usize => {
                    if GLOBAL_SCOPE_STATE
                        .compare_exchange(
                            GlobalScopeState::Uninitialized as usize,
                            owner_context,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        let initialization = GlobalInitialization::begin(owner_context);
                        initialization.publish(Scope::new());
                        return;
                    }
                }
                _ => core::hint::spin_loop(),
            }
        }
    }
}

fn current_context_identity() -> usize {
    let _guard = NoPreempt::new();
    // SAFETY: the guard keeps the current thread header stable while its opaque
    // identity is acquired. The header itself is pinned for the task lifetime,
    // so this identity remains valid if the task later migrates during an
    // initializer.
    let context = unsafe {
        ax_percpu::with_cpu_pin(|pin| pin.area().runtime_anchor().current_thread_raw())
            .expect("scope-local access requires an installed CPU area")
    };
    assert!(
        context > GlobalScopeState::Ready as usize,
        "scope-local initialization requires a valid current context"
    );
    context
}
