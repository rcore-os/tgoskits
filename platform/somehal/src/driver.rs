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
