use core::ptr::slice_from_raw_parts;

pub(super) fn append_linker_registers() {
    rdrive::register_append(registers());
}

fn registers() -> &'static [rdrive::register::DriverRegister] {
    let bytes = driver_registers();
    if bytes.is_empty() {
        return &[];
    }
    unsafe {
        core::slice::from_raw_parts(
            bytes.as_ptr().cast::<rdrive::register::DriverRegister>(),
            bytes.len() / core::mem::size_of::<rdrive::register::DriverRegister>(),
        )
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
