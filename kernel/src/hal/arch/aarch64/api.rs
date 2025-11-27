#[axvisor_api::api_mod_impl(axvisor_api::arch)]
mod arch_api_impl {
    use core::panic;

    use axvisor_api::memory::virt_to_phys;

    extern fn hardware_inject_virtual_interrupt(irq: axvisor_api::vmm::InterruptVector) {
        crate::hal::arch::inject_interrupt(irq as _);
    }

    extern fn read_vgicd_typer() -> u32 {
        let mut gic = rdrive::get_one::<rdif_intc::Intc>()
            .expect("Failed to get GIC driver")
            .lock()
            .unwrap();
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.typer_raw();
        }

        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            // Use the GICv3 driver to read the typer register
            return gic.typer_raw();
        }
        panic!("No GIC driver found");
    }

    extern fn read_vgicd_iidr() -> u32 {
        // use axstd::os::arceos::modules::axhal::irq::MyVgic;
        // MyVgic::get_gicd().lock().get_iidr()
        let mut gic = rdrive::get_one::<rdif_intc::Intc>()
            .expect("Failed to get GIC driver")
            .lock()
            .unwrap();
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.iidr_raw();
        }

        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            // Use the GICv3 driver to read the typer register
            return gic.iidr_raw();
        }

        panic!("No GIC driver found");
    }

    extern fn get_host_gicd_base() -> memory_addr::PhysAddr {
        let mut gic = rdrive::get_one::<rdif_intc::Intc>()
            .expect("Failed to get GIC driver")
            .lock()
            .unwrap();
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            let ptr: *mut u8 = gic.gicd_addr().as_ptr();
            return virt_to_phys((ptr as usize).into());
        }

        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            let ptr: *mut u8 = gic.gicd_addr().as_ptr();
            // Use the GICv3 driver to read the typer register
            return virt_to_phys((ptr as usize).into());
        }
        panic!("No GIC driver found");
    }

    extern fn get_host_gicr_base() -> memory_addr::PhysAddr {
        let mut gic = rdrive::get_one::<rdif_intc::Intc>()
            .expect("Failed to get GIC driver")
            .lock()
            .unwrap();
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            let ptr: *mut u8 = gic.gicr_addr().as_ptr();
            return virt_to_phys((ptr as usize).into());
        }
        panic!("No GICv3 driver found");
    }
}
