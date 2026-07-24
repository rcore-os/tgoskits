use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, mem, ptr::NonNull};

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

pub(crate) struct ItemBox {
    ptr: NonNull<Header>,
}

unsafe impl Send for ItemBox {}
// SAFETY: item descriptors are constructed only for `Send + Sync + 'static`
// payloads, and access to mutable scope slots remains exclusive.
unsafe impl Sync for ItemBox {}

impl ItemBox {
    pub(crate) fn new(item: &'static Item) -> Self {
        let (layout, offset) = layout(item.layout);
        let allocation = Allocation::new(layout);
        let ptr = allocation.ptr.cast::<Header>();

        unsafe {
            // The allocation uses the combined header/body layout. The item
            // descriptor guarantees that `init` writes exactly one payload
            // with the body's layout before the allocation is committed.
            ptr.write(Header { item });
            (item.init)(ptr.cast().byte_add(offset));
        }

        Self {
            ptr: allocation.commit().cast(),
        }
    }

    #[inline]
    fn header(&self) -> &Header {
        unsafe { self.ptr.as_ref() }
    }
}

struct Allocation {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl Allocation {
    fn new(layout: Layout) -> Self {
        let ptr =
            NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
        Self { ptr, layout }
    }

    fn commit(self) -> NonNull<u8> {
        let ptr = self.ptr;
        mem::forget(self);
        ptr
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: an uncommitted allocation still owns the exact pointer and
        // layout returned by `alloc`.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
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

#[cfg(all(axtest, feature = "axtest"))]
pub fn boxed_layout_rules_hold_for_test() -> bool {
    // layout: valid layouts succeed
    let l1 = Layout::new::<u8>();
    let (header_layout, offset1) = layout(l1);
    assert!(header_layout.size() >= core::mem::size_of::<Header>());
    assert!(offset1 >= core::mem::size_of::<Header>());
    
    // layout: larger type
    let l2 = Layout::new::<[u64; 8]>();
    let (header_layout2, offset2) = layout(l2);
    assert!(header_layout2.size() >= core::mem::size_of::<Header>());
    assert!(offset2 >= core::mem::size_of::<Header>());
    
    true
}

#[cfg(all(axtest, feature = "axtest"))]
pub fn boxed_layout_more_edge_cases_hold_for_test() -> bool {
    use core::alloc::Layout;
    
    // layout: zero-sized type
    let zst = Layout::new::<()>();
    let (header_layout_z, offset_z) = layout(zst);
    assert!(header_layout_z.size() >= core::mem::size_of::<Header>());
    // Offset should be at least Header size for ZST too
    assert!(offset_z >= core::mem::size_of::<Header>());
    
    // layout: u32 (4 bytes)
    let u32_layout = Layout::new::<u32>();
    let (_hl3, offset3) = layout(u32_layout);
    assert!(offset3 >= core::mem::size_of::<Header>());
    
    // layout: u64 (8 bytes)
    let u64_layout = Layout::new::<u64>();
    let (_hl4, offset4) = layout(u64_layout);
    assert!(offset4 >= core::mem::size_of::<Header>());
    
    // layout: aligned type (16-byte aligned)
    let aligned = Layout::from_size_align(16, 16).unwrap();
    let (header_layout_a, offset_a) = layout(aligned);
    assert!(header_layout_a.size() >= core::mem::size_of::<Header>());
    assert!(offset_a >= core::mem::size_of::<Header>());
    
    // Verify header alignment is reasonable
    assert!(header_layout_a.align() >= core::mem::align_of::<usize>());
    
    true
}

#[cfg(all(axtest, feature = "axtest"))]
pub fn boxed_layout_comprehensive_hold_for_test() -> bool {
    use core::alloc::Layout;
    
    // Test that layout always returns offset >= size_of::<Header>()
    let types_to_test: &[Layout] = &[
        Layout::new::<u8>(),
        Layout::new::<u16>(),
        Layout::new::<u32>(),
        Layout::new::<u64>(),
        Layout::new::<u128>(),
        Layout::new::<[u8; 1]>(),
        Layout::new::<[u8; 256]>(),
        Layout::new::<[u64; 16]>(),
        Layout::from_size_align(1, 1).unwrap(),
        Layout::from_size_align(1024, 1024).unwrap(),
        Layout::from_size_align(4096, 4096).unwrap(),
    ];
    
    for &t in types_to_test {
        let (_hl, offset) = layout(t);
        assert!(offset >= core::mem::size_of::<Header>());
    }
    
    // Header size should be reasonable (not zero, not huge)
    let header_size = core::mem::size_of::<Header>();
    assert!(header_size > 0);
    assert!(header_size <= 128);  // Should be small
    
    true
}

#[cfg(all(axtest, feature = "axtest"))]
pub fn boxed_header_size_and_alignment_hold_for_test() -> bool {
    use core::alloc::Layout;
    
    // Test Header struct properties
    let header_layout = Layout::new::<Header>();
    
    // Header should have reasonable size
    assert!(header_layout.size() > 0);
    assert!(header_layout.size() <= core::mem::size_of::<usize>() * 4);
    
    // Header alignment should be at least pointer-aligned
    assert!(header_layout.align() >= core::mem::align_of::<usize>());
    
    // Test that header layout is valid for allocation
    let (combined, _offset) = layout(Layout::new::<u8>());
    assert!(combined.size() > 0);
    
    // Test with various body sizes to ensure consistent behavior
    for size in [1, 2, 4, 8, 16, 32, 64, 128, 256, 512] {
        let body = Layout::from_size_align(size, 1).unwrap();
        let (_hl, offset) = layout(body);
        assert!(offset >= core::mem::size_of::<Header>());
        assert!(offset % core::mem::align_of::<usize>() == 0);  // Should be aligned
    }
    
    true
}

#[cfg(all(axtest, feature = "axtest"))]
pub fn boxed_layout_alignment_edge_cases_hold_for_test() -> bool {
    use core::alloc::Layout;
    
    // Test with various alignments
    for align in [1, 2, 4, 8, 16, 32, 64, 128] {
        let body = Layout::from_size_align(align, align).unwrap();
        let (hl, offset) = layout(body);
        
        // Combined layout should have at least the requested alignment
        assert!(hl.align() >= align);
        
        // Offset should be properly aligned
        assert!(offset >= core::mem::size_of::<Header>());
        assert!(offset % align == 0 || offset % core::mem::align_of::<Header>() == 0);
    }
    
    // Test that larger alignment doesn't break things
    let big_align = Layout::from_size_align(256, 256).unwrap();
    let (hl_big, offset_big) = layout(big_align);
    assert!(hl_big.align() >= 256);
    assert!(offset_big >= core::mem::size_of::<Header>());
    
    true
}
