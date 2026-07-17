use core::{
    alloc::Layout,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::{NonNull, addr_of},
};

use ax_kspin::PreemptGuard;
use ax_percpu::CpuPin;

use crate::{
    ScopeCell,
    scope::{ActiveScope, Scope, ScopeCellReadGuard, ScopeCellWriteGuard, ScopeItemLease},
};

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
    /// The current CPU stays pinned for the complete operation. The
    /// higher-ranked closure prevents a reference into the selected scope from
    /// escaping after the pin is released.
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
        let guard = PreemptGuard::new();
        self.with_pinned(guard.cpu_pin(), operation)
    }

    /// Runs `operation` with the current value under an existing CPU pin.
    ///
    /// It never enters or leaves preemption state itself. First access to an
    /// item can allocate, so hard-IRQ callers must use
    /// [`Self::try_with_pinned`] after task context has initialized the item.
    /// The caller remains responsible for making `operation` valid in the
    /// context represented by `pin`.
    pub fn with_pinned<R>(
        &self,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access T) -> R,
    ) -> R {
        ActiveScope::with_item(self.item, pin, |item| operation(item.as_ref()))
    }

    /// Runs `operation` under an existing CPU pin without lazy initialization.
    ///
    /// Returns `None` when this item has not been initialized in the active
    /// scope. Once initialized in task context, this path performs no
    /// allocation, context transition, or user callback other than
    /// `operation`, making it suitable for a caller holding an IRQ-derived pin
    /// when that operation is itself hard-IRQ-safe.
    pub fn try_with_pinned<R>(
        &self,
        pin: &CpuPin,
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

    /// Initializes or mutates this item before a [`ScopeCell`] is published.
    ///
    /// The exclusive cell reference proves that ordinary readers cannot exist,
    /// and the cell additionally rejects scheduler-active bindings. Unlike
    /// [`Self::scope_cell_mut`], this construction-only path does not enter a
    /// preemption context or acquire the scope gate. Initializers may allocate;
    /// callers must finish all such work before publishing the cell to a task.
    pub fn scope_cell_mut_unpublished<'scope>(
        &self,
        scope: &'scope mut ScopeCell,
    ) -> ScopeItemMut<'scope, T> {
        ScopeItemMut {
            item: scope.get_mut_unpublished(self.item),
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
                        unsafe { ptr.cast().write(val) }
                    }, |ptr| unsafe {
                        ptr.cast::<$ty>().drop_in_place();
                    })
                };

                unsafe { $crate::LocalItem::new(&ITEM) }
            };
        )+
    }
}
