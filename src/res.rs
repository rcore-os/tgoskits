use core::{
    alloc::Layout,
    marker::PhantomData,
    ops::Deref,
    ptr::{NonNull, addr_of},
};

use crate::{Namespace, arc::ResArc};

#[doc(hidden)]
pub struct Resource {
    pub layout: Layout,
    pub init: fn(NonNull<()>),
    pub drop: fn(NonNull<()>),
}

// Mimic `linkme`
pub(crate) struct Resources;

impl Deref for Resources {
    type Target = [Resource];

    fn deref(&self) -> &Self::Target {
        unsafe extern "Rust" {
            #[link_name = "__start_axns_resources"]
            static RESOURCES_START: Resource;
            #[link_name = "__stop_axns_resources"]
            static RESOURCES_STOP: Resource;
        }
        let start = addr_of!(RESOURCES_START) as usize;
        let len = (addr_of!(RESOURCES_STOP) as usize - start) / core::mem::size_of::<Resource>();
        unsafe { core::slice::from_raw_parts(start as *const Resource, len) }
    }
}

impl Resource {
    #[inline]
    pub(crate) fn index(&'static self) -> usize {
        // FIXME: offset_from_unsigned is not available on nightly-2025-01-18
        // unsafe { (self as *const Resource).offset_from_unsigned(Resources.as_ptr()) }
        (self as *const Resource as usize - Resources.as_ptr() as usize)
            / core::mem::size_of::<Resource>()
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
            $(#[$attr])*
            $vis static $name: $crate::ResWrapper<$ty> = {
                #[unsafe(link_section = "axns_resources")]
                static RES: $crate::Resource = $crate::Resource {
                    layout: core::alloc::Layout::new::<$ty>(),
                    init: |ptr| {
                        let val: $ty = $default;
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
