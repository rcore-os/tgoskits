use core::ptr::slice_from_raw_parts;

use rdrive::register::DriverRegisterSlice;

pub(crate) fn append_linker_registers() {
    let registers = DriverRegisterSlice::from_raw(driver_registers());
    if !registers.is_empty() {
        rdrive::register_append(&registers);
    }
}

fn driver_registers() -> &'static [u8] {
    unsafe extern "C" {
        fn __sdriver_register();
        fn __edriver_register();
    }

    unsafe {
        &*slice_from_raw_parts(
            __sdriver_register as *const () as *const u8,
            __edriver_register as *const () as usize - __sdriver_register as *const () as usize,
        )
    }
}
