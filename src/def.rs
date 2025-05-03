use core::{alloc::Layout, marker::PhantomData, ops::Deref, ptr::NonNull};

use crate::{Namespace, arc::ResArc};

pub struct Resource {
    pub layout: Layout,
    pub init: fn(NonNull<()>),
    pub drop: fn(NonNull<()>),
}

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

    #[inline]
    pub fn current(&self) -> ResCurrent<T> {
        ResCurrent {
            res: self.res,
            ns: crate::current_ns(),
            _p: PhantomData,
        }
    }

    #[inline]
    pub fn get<'ns>(&self, ns: &'ns Namespace) -> &'ns T {
        ns.get(self.res).as_ref()
    }

    #[inline]
    pub fn get_mut<'ns>(&self, ns: &'ns mut Namespace) -> Option<&'ns mut T> {
        ns.get_mut(self.res).get_mut()
    }

    #[inline]
    pub fn clone_from<'ns>(&self, dst: &'ns mut Namespace, src: &'ns Namespace) {
        *dst.get_mut(self.res) = src.get(self.res).clone();
    }

    #[inline]
    pub fn reset(&self, ns: &mut Namespace) {
        *ns.get_mut(self.res) = ResArc::new(self.res);
    }
}

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

#[macro_export]
macro_rules! def_resource {
    ( $( $(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $default:expr; )+ ) => {
        $(
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
            const _: () = assert!(RES.layout.size() != 0, "Resource has zero size");

            #[used]
            #[doc(hidden)]
            $(#[$attr])*
            $vis static $name: $crate::ResWrapper<$ty> = $crate::ResWrapper::new(&RES);
        )+
    }
}
