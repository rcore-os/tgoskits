use os_helper::memory::MemoryDescriptor;

pub fn setup_allocator(regions: &[MemoryDescriptor]) {
    crate::os::mem::init_heap(regions);
    crate::os::logger::init();
}

pub fn setup() -> ! {
    unsafe extern "C" {
        fn __sparreal_main() -> !;
    }

    unsafe { __sparreal_main() }
}
