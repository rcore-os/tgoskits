use core::ptr::NonNull;

pub fn rdrive_setup() {
    if let Some(addr) = someboot::fdt_addr() {
        info!("Initializing rdrive with FDT at {:?}", addr);
        rdrive::init(rdrive::Platform::Fdt {
            addr: NonNull::new(addr).unwrap(),
        })
        .unwrap();
    }
}
