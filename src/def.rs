use core::{alloc::Layout, marker::PhantomData, ops::Deref, ptr::NonNull};

use crate::{Namespace, arc::ResArc};

#[doc(hidden)]
pub struct Resource {
    pub layout: Layout,
    pub init: fn(NonNull<()>),
    pub drop: fn(NonNull<()>),
}

#[doc(hidden)]
#[linkme::distributed_slice]
pub static RESOURCES: [Resource];

impl Resource {
    #[inline]
    pub(crate) fn index(&'static self) -> usize {
        // FIXME: offset_from_unsigned is not available on nightly-2025-01-18
        // unsafe { (self as *const Resource).offset_from_unsigned(RESOURCES.as_ptr()) }
        (self as *const _ as usize - RESOURCES.as_ptr() as usize) / core::mem::size_of::<Resource>()
    }
}

/// A wrapper around a resource.
///
/// This is used to access the resource in a namespace.
///
/// It is created by the [`def_resource!`] macro.
///
/// [`def_resource!`]: crate::def_resource
pub struct ResWrapper<T> {
    res: &'static Resource,
    _p: PhantomData<T>,
}

impl<T> ResWrapper<T> {
    #[doc(hidden)]
    #[inline]
    pub const fn new(res: &'static Resource) -> Self {
        Self {
            res,
            _p: PhantomData,
        }
    }

    /// Access the resource in the current namespace.
    #[inline]
    pub fn current(&self) -> ResCurrent<T> {
        ResCurrent {
            res: self.res,
            ns: crate::current_ns(),
            _p: PhantomData,
        }
    }

    /// Get a reference to the resource in the given namespace.
    #[inline]
    pub fn get<'ns>(&self, ns: &'ns Namespace) -> &'ns T {
        ns.get(self.res).as_ref()
    }

    /// Get a mutable reference to the resource in the given namespace.
    #[inline]
    pub fn get_mut<'ns>(&self, ns: &'ns mut Namespace) -> Option<&'ns mut T> {
        ns.get_mut(self.res).get_mut()
    }

    /// Share the resource from one namespace to another.
    #[inline]
    pub fn share_from<'ns>(&self, dst: &'ns mut Namespace, src: &'ns Namespace) {
        *dst.get_mut(self.res) = src.get(self.res).clone();
    }

    /// Reset the resource in the given namespace to its default value.
    #[inline]
    pub fn reset(&self, ns: &mut Namespace) {
        *ns.get_mut(self.res) = ResArc::new(self.res);
    }
}

/// A wrapper around a resource in the current namespace.
///
/// This struct provides access to the resource in the namespace
/// that is currently active.
///
/// Note that this type is needed since the requirement of [`CurrentNs`].
///
/// [`CurrentNs`]: crate::CurrentNs
pub struct ResCurrent<T> {
    res: &'static Resource,
    ns: crate::CurrentNsImpl,
    _p: PhantomData<T>,
}

impl<T> Deref for ResCurrent<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ns.as_ref().get(self.res).as_ref()
    }
}

/// Define resources.
///
/// # Example
///
/// ```
/// # use std::sync::atomic::AtomicUsize;
/// # use axns::def_resource;
/// def_resource! {
///     /// A static integer resource.
///     pub static MY_RESOURCE: i32 = 42;
///     /// An atomic integer resource.
///     pub static MY_ATOMIC_RESOURCE: AtomicUsize = AtomicUsize::new(0);
/// }
/// ```
#[macro_export]
macro_rules! def_resource {
    ( $( $(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $default:expr; )+ ) => {
        $(
            #[used]
            #[doc(hidden)]
            $(#[$attr])*
            $vis static $name: $crate::ResWrapper<$ty> = {
                #[linkme::distributed_slice($crate::RESOURCES)]
                static RES: $crate::Resource = $crate::Resource {
                    layout: core::alloc::Layout::new::<$ty>(),
                    init: |ptr| {
                        let val = $default;
                        unsafe { ptr.cast().write(val) }
                    },
                    drop: |ptr| unsafe {
                        ptr.cast::<$ty>().drop_in_place();
                    },
                };

                assert!(RES.layout.size() != 0, "Resource has zero size");

                $crate::ResWrapper::new(&RES)
            };
        )+
    }
}
