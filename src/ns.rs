use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, iter::zip, mem::MaybeUninit, ptr::NonNull};

use crate::{RESOURCES, Resource, arc::ResArc};

pub struct Namespace {
    // Not using [ResArc<A>] to save a `usize` because we know the length
    ptr: NonNull<ResArc>,
}

unsafe impl Send for Namespace {}
unsafe impl Sync for Namespace {}

impl Namespace {
    fn layout() -> Layout {
        Layout::array::<ResArc>(RESOURCES.len()).unwrap()
    }

    pub fn new() -> Self {
        let layout = Self::layout();
        let ptr = NonNull::new(unsafe { alloc(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout))
            .cast();

        let slice = unsafe {
            core::slice::from_raw_parts_mut(ptr.cast::<MaybeUninit<_>>().as_ptr(), RESOURCES.len())
        };
        for (res, d) in zip(RESOURCES, slice) {
            d.write(ResArc::new(res));
        }

        Self { ptr }
    }

    pub(crate) fn get(&self, res: &'static Resource) -> &ResArc {
        let index = res.index();
        unsafe { self.ptr.add(index).as_ref() }
    }

    pub(crate) fn get_mut(&mut self, res: &'static Resource) -> &mut ResArc {
        let index = res.index();
        unsafe { self.ptr.add(index).as_mut() }
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        let ptr = NonNull::slice_from_raw_parts(self.ptr, RESOURCES.len());
        unsafe {
            ptr.drop_in_place();
            dealloc(self.ptr.cast().as_ptr(), Self::layout());
        }
    }
}
