use crate::hal::al;

pub fn start_kernel() -> ! {
    crate::os::logger::init();
    info!("Setting up allocator...");

    crate::os::mem::init_heap(al::memory::memory_map());
    al::platform::post_allocator();
    crate::os::mem::paging::init();

    al::platform::post_paging();

    crate::os::time::init();

    // rdrive::probe_all(true).unwrap();

    al::cpu::irq_local_set_enable(true);

    unsafe extern "C" {
        fn __sparreal_main();
    }

    unsafe { __sparreal_main() };

    al::platform::shutdown()
}
