mod api;
pub mod cache;

use crate::vmm::vm_list::get_vm_by_id;
use axaddrspace::{GuestPhysAddr, device::AccessWidth};
use axvisor_api::vmm::current_vm_id;

pub fn hardware_check() {
    // TODO: implement hardware checks for RISC-V64
    // check page table level like aarch64

}

pub fn inject_interrupt(irq_id: usize) {
    debug!("injecting interrupt id: {}", irq_id);

    // Get the instance of the vplic, and then inject virtual interrupt.
    let vplic = get_vm_by_id(current_vm_id()).unwrap().get_devices().find_mmio_dev(GuestPhysAddr::from_usize(axruntime::plic_base())).unwrap();
    
    // Calulate the pending register offset and value.
    let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
    let addr = GuestPhysAddr::from_usize(axruntime::plic_base() + reg_offset);
    let width = AccessWidth::Dword;
    let val: u32 = 1 << (irq_id % 32);

    // Use a trick write to set the pending bit.
    let _ = vplic.handle_write(addr, width, val as _);
}
