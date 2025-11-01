use some_serial::{Serial, ns16550};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kernel_entry() -> ! {
    unimplemented!()
}

pub(crate) fn efi_kernel_prepare() {
    println!("Preparing kernel entry...");

    let addr = 0x1FE001E0usize;
    unsafe{
        let ptr = addr as *mut u8;
        core::ptr::write_volatile(ptr, b'A');
        core::ptr::write_volatile(ptr, b'\r');
        core::ptr::write_volatile(ptr, b'\n');
    }


}


 