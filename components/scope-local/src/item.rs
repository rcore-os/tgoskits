use core::{
    alloc::Layout,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::{NonNull, addr_of},
};

use ax_kernel_guard::NoPreempt;
use ax_percpu::CpuPin;

use crate::scope::{ActiveScope, Scope, ScopeCellReadGuard, ScopeCellWriteGuard, ScopeItemLease};

#[doc(hidden)]
pub struct Item {
    pub(crate) layout: Layout,
    pub(crate) init: fn(NonNull<()>),
    pub(crate) drop: fn(NonNull<()>),
}

pub(crate) struct Registry;

impl Deref for Registry {
    type Target = [Item];

    fn deref(&self) -> &Self::Target {
        unsafe extern "Rust" {
            static __start_scope_local: Item;
            static __stop_scope_local: Item;
        }
        let start = addr_of!(__start_scope_local) as usize;
        let len = (addr_of!(__stop_scope_local) as usize - start) / core::mem::size_of::<Item>();
        unsafe { core::slice::from_raw_parts(start as *const Item, len) }
    }
}

impl Item {
    /// Creates one type-erased registry descriptor.
    ///
    /// # Safety
    ///
    /// `init` must initialize exactly one valid `T` at the supplied aligned
    /// address, and `drop` must drop exactly that value without deallocating
    /// its storage. The descriptor must only be paired with `LocalItem<T>`.
    #[doc(hidden)]
    pub const unsafe fn new<T: Send + Sync + 'static>(
        init: fn(NonNull<()>),
        drop: fn(NonNull<()>),
    ) -> Self {
        Self {
            layout: Layout::new::<T>(),
            init,
            drop,
        }
    }

    #[inline]
    pub(crate) fn index(&'static self) -> usize {
        unsafe { (self as *const Item).offset_from_unsigned(Registry.as_ptr()) }
    }
}

/// A scope-local item.
pub struct LocalItem<T> {
    item: &'static Item,
    _p: PhantomData<T>,
}

impl<T: Send + Sync + 'static> LocalItem<T> {
    #[doc(hidden)]
    #[inline]
    /// # Safety
    ///
    /// `item` must have been created for exactly `T` and its initializer and
    /// destructor must obey [`Item::new`]'s contract.
    pub const unsafe fn new(item: &'static Item) -> Self {
        Self {
            item,
            _p: PhantomData,
        }
    }

    /// Runs `operation` with the value selected by the current active scope.
    ///
    /// The higher-ranked closure prevents a reference into per-CPU-selected
    /// storage from escaping after preemption is re-enabled. The first global
    /// access initializes the global scope before entering the pinned access.
    /// Concurrent first access waits for that initialization to be published.
    ///
    /// This entry is intended for task context. Callers that already hold an
    /// IRQ or preemption guard should use [`Self::with_pinned`] to avoid a
    /// context transition on return. `operation` must not block, sleep, yield,
    /// or retain another context-aware guard; clone an owned handle and perform
    /// potentially blocking work after this method returns instead.
    ///
    /// ```compile_fail
    /// use scope_local::scope_local;
    ///
    /// scope_local! {
    ///     static VALUE: usize = 1;
    /// }
    ///
    /// let escaped: &'static usize = VALUE.with(|value| value);
    /// ```
    pub fn with<R>(&self, operation: impl for<'access> FnOnce(&'access T) -> R) -> R {
        let mut operation = Some(operation);
        loop {
            let guard = NoPreempt::new();
            // SAFETY: `NoPreempt` prevents migration for this complete access.
            let result = unsafe {
                ax_percpu::with_cpu_pin(|pin| {
                    ActiveScope::try_with_item(self.item, pin, |item| {
                        let operation = operation
                            .take()
                            .expect("scope-local operation must run at most once");
                        operation(item.as_ref())
                    })
                })
            }
            .expect("scope-local access requires an installed CPU area");
            drop(guard);

            if let Some(result) = result {
                return result;
            }
            ActiveScope::initialize_global();
        }
    }

    /// Runs `operation` with the current value under an existing CPU pin.
    ///
    /// It never enters or leaves preemption state itself. The selected global
    /// scope must already have been initialized by [`Self::with`]; explicit
    /// [`Scope`] values are initialized eagerly. The caller remains responsible
    /// for making `operation` valid in the context represented by `pin`.
    pub fn with_pinned<'pin, R>(
        &self,
        pin: &CpuPin<'pin>,
        operation: impl for<'access> FnOnce(&'access T) -> R,
    ) -> R {
        ActiveScope::with_item(self.item, pin, |item| operation(item.as_ref()))
    }

    /// Runs `operation` under an existing CPU pin without lazy initialization.
    ///
    /// Returns `None` when the global scope has not been initialized. This path
    /// performs no allocation, lock acquisition, context transition, or user
    /// callback other than `operation`, making it suitable for a caller holding
    /// an IRQ-derived pin when that operation is itself hard-IRQ-safe.
    pub fn try_with_pinned<'pin, R>(
        &self,
        pin: &CpuPin<'pin>,
        operation: impl for<'access> FnOnce(&'access T) -> R,
    ) -> Option<R> {
        ActiveScope::try_with_item(self.item, pin, |item| operation(item.as_ref()))
    }

    /// Clones the value selected by the current active scope.
    ///
    /// This is the preferred entry for `Arc`-backed lock owners: the CPU pin is
    /// released before the returned owner is locked or used by potentially
    /// blocking code.
    pub fn clone_current(&self) -> T
    where
        T: Clone,
    {
        self.with(Clone::clone)
    }

    /// Returns a reference to this item within the given scope.
    pub fn scope<'scope>(&self, scope: &'scope Scope) -> ScopeItem<'scope, T> {
        ScopeItem {
            lease: scope.read_item(self.item),
            _p: PhantomData,
        }
    }

    /// Returns a mutable reference to this item within the given scope.
    pub fn scope_mut<'scope>(&self, scope: &'scope mut Scope) -> ScopeItemMut<'scope, T> {
        ScopeItemMut {
            item: scope.get_mut_unlocked(self.item),
            _p: PhantomData,
        }
    }

    /// Returns the value selected through an existing [`ScopeCell`] read
    /// capability.
    ///
    /// This path reuses the guard's shared count. It never recursively acquires
    /// the underlying gate, so a writer that has already published upgrade
    /// intent cannot deadlock the current reader.
    ///
    /// [`ScopeCell`]: crate::ScopeCell
    pub fn scope_cell<'scope>(&self, scope: &'scope ScopeCellReadGuard<'_>) -> &'scope T {
        scope.get(self.item).as_ref()
    }

    /// Returns mutable access to this item under a [`ScopeCell`] writer guard.
    ///
    /// Unlike [`Self::scope_mut`], this path never creates `&mut Scope`; the
    /// guard authorizes slot-level interior mutation while other CPUs may still
    /// retain the stable active-scope identity.
    ///
    /// [`ScopeCell`]: crate::ScopeCell
    pub fn scope_cell_mut<'scope>(
        &self,
        scope: &'scope mut ScopeCellWriteGuard<'_>,
    ) -> ScopeItemMut<'scope, T> {
        ScopeItemMut {
            item: scope.get_mut(self.item),
            _p: PhantomData,
        }
    }
}

/// A reference to a scope-local item within a specific scope.
///
/// Created by [`LocalItem::scope`].
pub struct ScopeItem<'scope, T> {
    lease: ScopeItemLease<'scope>,
    _p: PhantomData<T>,
}

impl<'scope, T> Deref for ScopeItem<'scope, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.lease.item().as_ref()
    }
}

/// A mutable reference to a scope-local item within a specific scope.
///
/// Created by [`LocalItem::scope_mut`].
pub struct ScopeItemMut<'scope, T> {
    item: &'scope mut crate::boxed::ItemBox,
    _p: PhantomData<T>,
}

impl<'scope, T> Deref for ScopeItemMut<'scope, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.item.as_ref()
    }
}

impl<'scope, T> DerefMut for ScopeItemMut<'scope, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.item.as_mut()
    }
}

/// Define a scope-local item.
///
/// # Example
///
/// ```
/// # use std::sync::atomic::AtomicUsize;
/// # use scope_local::scope_local;
/// scope_local! {
///     /// An integer.
///     pub static MY_I32: i32 = 42;
///     /// An atomic integer.
///     pub static MY_ATOMIC_USIZE: AtomicUsize = AtomicUsize::new(0);
/// }
/// ```
#[macro_export]
macro_rules! scope_local {
    ( $( $(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $default:expr; )+ ) => {
        $(
            $(#[$attr])*
            $vis static $name: $crate::LocalItem<$ty> = {
                #[unsafe(link_section = "scope_local")]
                static ITEM: $crate::Item = unsafe {
                    $crate::Item::new::<$ty>(|ptr| {
                        let val: $ty = $default;
                        ptr.cast().write(val)
                    }, |ptr| {
                        ptr.cast::<$ty>().drop_in_place();
                    })
                };

                unsafe { $crate::LocalItem::new(&ITEM) }
            };
        )+
    }
}
