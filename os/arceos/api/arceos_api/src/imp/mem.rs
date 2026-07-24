use core::alloc::Layout;

cfg_alloc! {
    use core::ptr::NonNull;

    pub unsafe fn ax_alloc(layout: Layout) -> Option<NonNull<u8>> {
        ax_alloc::global_allocator().alloc(layout).ok()
    }

    pub unsafe fn ax_dealloc(ptr: NonNull<u8>, layout: Layout) {
        ax_alloc::global_allocator().dealloc(ptr, layout)
    }
}
