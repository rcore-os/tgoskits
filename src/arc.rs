use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
        fence,
    },
};

use crate::Resource;

const MAX_REFCOUNT: usize = (isize::MAX) as usize;
const INTERNAL_OVERFLOW_ERROR: &str = "ResArc counter overflow";

#[repr(C)]
struct ResInner {
    res: &'static Resource,
    strong: AtomicUsize,
}

fn layout(body: Layout) -> (Layout, usize) {
    Layout::new::<ResInner>()
        .extend(body)
        .unwrap_or_else(|_| handle_alloc_error(body))
}

impl ResInner {
    #[inline]
    fn body(&self) -> NonNull<()> {
        let (_, offset) = layout(self.res.layout);
        unsafe {
            // FIXME: `NonNull::from_ref` is not stable yet
            // NonNull::from_ref(self)
            NonNull::new_unchecked(self as *const Self as *mut Self)
                .cast::<()>()
                .byte_add(offset)
        }
    }
}

impl Drop for ResInner {
    fn drop(&mut self) {
        (self.res.drop)(self.body());
    }
}

pub(crate) struct ResArc {
    ptr: NonNull<ResInner>,
}

unsafe impl Send for ResArc {}
unsafe impl Sync for ResArc {}

impl ResArc {
    pub(crate) fn new(res: &'static Resource) -> Self {
        let (layout, offset) = layout(res.layout);
        let ptr = NonNull::new(unsafe { alloc(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout))
            .cast();

        unsafe {
            ptr.write(ResInner {
                res,
                strong: AtomicUsize::new(1),
            });
            (res.init)(ptr.cast().byte_add(offset));
        }

        Self { ptr }
    }

    #[inline]
    fn inner(&self) -> &ResInner {
        unsafe { self.ptr.as_ref() }
    }

    pub(crate) fn get_mut<T>(&mut self) -> Option<&mut T> {
        if self.inner().strong.load(Acquire) == 1 {
            Some(unsafe { self.inner().body().cast().as_mut() })
        } else {
            None
        }
    }
}

impl<T> AsRef<T> for ResArc {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { self.inner().body().cast().as_ref() }
    }
}

impl Clone for ResArc {
    fn clone(&self) -> Self {
        let old_size = self.inner().strong.fetch_add(1, Relaxed);
        assert!(old_size <= MAX_REFCOUNT, "{}", INTERNAL_OVERFLOW_ERROR);

        Self { ptr: self.ptr }
    }
}

impl Drop for ResArc {
    fn drop(&mut self) {
        if self.inner().strong.fetch_sub(1, Release) != 1 {
            return;
        }

        fence(Acquire);

        let res = self.inner().res;
        let (layout, offset) = layout(res.layout);

        unsafe {
            (res.drop)(self.ptr.cast().byte_add(offset));
            dealloc(self.ptr.cast().as_ptr(), layout);
        }
    }
}
