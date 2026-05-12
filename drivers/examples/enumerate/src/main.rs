use std::ptr::NonNull;

use log::debug;
use rdrive::get_list;

use crate::intc::IrqTest;

pub mod blk;
// pub mod clk;
pub mod intc;
// pub mod timer;

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();

    let fdt = include_bytes!("../../../data/qemu.dtb");

    rdrive::init(rdrive::Platform::Fdt {
        addr: NonNull::new(fdt.as_ptr() as usize as _).unwrap(),
    })
    .unwrap();

    rdrive::register_add(intc::register());
    // rdrive::register_add(timer::register());
    // rdrive::register_add(clk::register());
    rdrive::register_add(blk::register());

    rdrive::probe_pre_kernel().unwrap();

    let intc_list = get_list::<rdif_intc::Intc>();
    for intc in intc_list {
        println!("intc: {:?}", intc.descriptor());

        let g = intc.lock().unwrap();

        let t = g.typed_ref::<IrqTest>();
        debug!("intc: {:?}", intc.descriptor().name);

        assert!(t.is_some(), "Intc should be [IrqTest]");
    }

    rdrive::probe_all(true).unwrap();
    println!("--- after probe all ---");
    rdrive::probe_all(true).unwrap();

    let id = rdrive::fdt_phandle_to_device_id(0x8000.into());

    println!("phandle 0x8000 to device id: {:?}", id);
}
