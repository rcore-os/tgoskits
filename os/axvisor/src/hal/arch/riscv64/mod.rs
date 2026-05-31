mod api;
pub mod cache;

use axaddrspace::{GuestPhysAddr, device::AccessWidth};
use axvisor_api::vmm::current_vm_id;
use axvisor_core::vmm::vm_list::get_vm_by_id;

const GUEST_PLIC_PADDR: usize = 0x0c00_0000;

pub fn hardware_check() {
    api::init_platform_irq_injector();
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64
}

pub fn inject_interrupt(irq_id: usize) {
    debug!("injecting interrupt id: {}", irq_id);

    // Get the instance of the vplic, and then inject virtual interrupt.
    let vplic = get_vm_by_id(current_vm_id())
        .unwrap()
        .get_devices()
        .find_mmio_dev(GuestPhysAddr::from_usize(GUEST_PLIC_PADDR))
        .unwrap();

    // Calulate the pending register offset and value.
    let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
    let addr = GuestPhysAddr::from_usize(GUEST_PLIC_PADDR + reg_offset);
    let width = AccessWidth::Dword;
    let val: u32 = 1 << (irq_id % 32);

    // Use a trick write to set the pending bit.
    let _ = vplic.handle_write(addr, width, val as _);
}
