use core::ptr::{NonNull, slice_from_raw_parts};

use rdrive::register::DriverRegisterSlice;

pub fn rdrive_setup() {
    let registers = DriverRegisterSlice::from_raw(driver_registers());

    if let Some(addr) = someboot::fdt_addr() {
        info!("Initializing rdrive with FDT at {:?}", addr);
        rdrive::init(rdrive::Platform::Fdt {
            addr: NonNull::new(addr).unwrap(),
        })
        .unwrap();

        rdrive::register_append(&registers);

        rdrive::probe_pre_kernel().unwrap();
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
