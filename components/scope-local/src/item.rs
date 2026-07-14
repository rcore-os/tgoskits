use core::{
    alloc::Layout,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::{NonNull, addr_of},
};

use ax_kspin::PreemptGuard;
use ax_percpu::CpuPin;

use crate::scope::{ActiveScope, Scope};

#[doc(hidden)]
pub struct Item {
    pub layout: Layout,
    pub init: fn(NonNull<()>),
    pub drop: fn(NonNull<()>),
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

impl<T> LocalItem<T> {
    #[doc(hidden)]
    #[inline]
    pub const fn new(item: &'static Item) -> Self {
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
            item: self.item,
            scope,
            _p: PhantomData,
        }
    }

    /// Returns a mutable reference to this item within the given scope.
    pub fn scope_mut<'scope>(&self, scope: &'scope mut Scope) -> ScopeItemMut<'scope, T> {
        ScopeItemMut {
            item: self.item,
            scope,
            _p: PhantomData,
        }
    }
}

/// A reference to a scope-local item within a specific scope.
///
/// Created by [`LocalItem::scope`].
pub struct ScopeItem<'scope, T> {
    item: &'static Item,
    scope: &'scope Scope,
    _p: PhantomData<T>,
}

impl<'scope, T> Deref for ScopeItem<'scope, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.scope.get(self.item).as_ref()
    }
}

/// A mutable reference to a scope-local item within a specific scope.
///
/// Created by [`LocalItem::scope_mut`].
pub struct ScopeItemMut<'scope, T> {
    item: &'static Item,
    scope: &'scope mut Scope,
    _p: PhantomData<T>,
}

impl<'scope, T> Deref for ScopeItemMut<'scope, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.scope.get(self.item).as_ref()
    }
}

impl<'scope, T> DerefMut for ScopeItemMut<'scope, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.scope.get_mut(self.item).as_mut()
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
                static ITEM: $crate::Item = $crate::Item {
                    layout: core::alloc::Layout::new::<$ty>(),
                    init: |ptr| {
                        let val: $ty = $default;
                        unsafe { ptr.cast().write(val) }
                    },
                    drop: |ptr| unsafe {
                        ptr.cast::<$ty>().drop_in_place();
                    },
                };

                $crate::LocalItem::new(&ITEM)
            };
        )+
    }
}
