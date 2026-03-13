use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, ptr::NonNull};

use crate::item::Item;

#[repr(C)]
struct Header {
    item: &'static Item,
}

fn layout(body: Layout) -> (Layout, usize) {
    Layout::new::<Header>()
        .extend(body)
        .unwrap_or_else(|_| handle_alloc_error(body))
}

impl Header {
    #[inline]
    fn body(&self) -> NonNull<()> {
        let (_, offset) = layout(self.item.layout);
        unsafe {
            // FIXME: `NonNull::from_ref` is not stable yet
            NonNull::new_unchecked(self as *const Self as *mut Self)
                .cast::<()>()
                .byte_add(offset)
        }
    }
}

impl Drop for Header {
    fn drop(&mut self) {
        (self.item.drop)(self.body());
    }
}

pub(crate) struct ItemBox {
    ptr: NonNull<Header>,
}

unsafe impl Send for ItemBox {}
unsafe impl Sync for ItemBox {}

impl ItemBox {
    pub(crate) fn new(item: &'static Item) -> Self {
        let (layout, offset) = layout(item.layout);
        let ptr = NonNull::new(unsafe { alloc(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout))
            .cast();

        unsafe {
            ptr.write(Header { item });
            (item.init)(ptr.cast().byte_add(offset));
        }

        Self { ptr }
    }

    #[inline]
    fn header(&self) -> &Header {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> AsRef<T> for ItemBox {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { self.header().body().cast().as_ref() }
    }
}

impl<T> AsMut<T> for ItemBox {
    #[inline]
    fn as_mut(&mut self) -> &mut T {
        unsafe { self.header().body().cast().as_mut() }
    }
}

impl Drop for ItemBox {
    fn drop(&mut self) {
        let item = self.header().item;
        let (layout, offset) = layout(item.layout);
        unsafe {
            (item.drop)(self.ptr.cast().byte_add(offset));
            dealloc(self.ptr.cast().as_ptr(), layout);
        }
    }
}
