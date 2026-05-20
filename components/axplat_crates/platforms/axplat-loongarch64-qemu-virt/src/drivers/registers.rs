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
        fn _sdriver();
        fn _edriver();
    }

    unsafe {
        &*slice_from_raw_parts(
            _sdriver as *const () as *const u8,
            _edriver as *const () as usize - _sdriver as *const () as usize,
        )
    }
}
