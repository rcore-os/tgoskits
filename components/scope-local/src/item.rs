use core::{
    alloc::Layout,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::{NonNull, addr_of},
};

use ax_kernel_guard::NoPreempt;
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
    /// Creates one type-erased registry descriptor for `T`.
    ///
    /// # Safety
    ///
    /// `init` must initialize exactly one valid `T` at the supplied address,
    /// and `drop` must destroy that value without deallocating its storage.
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
    /// `item` must have been constructed for exactly `T`.
    pub const unsafe fn new(item: &'static Item) -> Self {
        Self {
            item,
            _p: PhantomData,
        }
    }

    /// Runs `operation` with the value selected by the active scope.
    ///
    /// The higher-ranked closure prevents a reference into per-CPU-selected
    /// storage from escaping after preemption is re-enabled. The first global
    /// access initializes the global scope before entering the pinned access.
    /// Concurrent first access waits for that initialization to be published.
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

    /// Runs `operation` under an existing CPU pin without initialization.
    ///
    /// # Panics
    ///
    /// Panics if the selected global scope has not been initialized by
    /// [`LocalItem::with`]. Explicit [`Scope`] values are initialized eagerly.
    pub fn with_pinned<R>(
        &self,
        pin: &CpuPin<'_>,
        operation: impl for<'access> FnOnce(&'access T) -> R,
    ) -> R {
        ActiveScope::with_item(self.item, pin, |item| operation(item.as_ref()))
    }

    /// Runs `operation` without lazy initialization under an existing pin.
    ///
    /// This returns `None` when the selected global scope has not yet been
    /// initialized, allowing hard-IRQ callers to avoid allocation.
    pub fn try_with_pinned<R>(
        &self,
        pin: &CpuPin<'_>,
        operation: impl for<'access> FnOnce(&'access T) -> R,
    ) -> Option<R> {
        ActiveScope::try_with_item(self.item, pin, |item| operation(item.as_ref()))
    }

    /// Clones the selected value while keeping the CPU pin lifetime short.
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
